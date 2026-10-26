use std::io::{self, IsTerminal, Write};

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Args;
use usque_cloudflare_api::{self, CloudflareClient};
use usque_config::{parse_endpoint_v4, parse_endpoint_v6, Config, DEFAULT_ENDPOINT_H2_V4};
use usque_crypto::generate_ec_keypair;

#[derive(Debug, Args, Clone)]
pub struct RegisterArgs {
    #[arg(short = 'l', long, default_value = usque_cloudflare_api::DEFAULT_LOCALE)]
    pub locale: String,
    #[arg(short = 'm', long, default_value = usque_cloudflare_api::DEFAULT_MODEL)]
    pub model: String,
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    #[arg(long)]
    pub jwt: Option<String>,
    #[arg(short = 'a', long = "accept-tos")]
    pub accept_tos: bool,
    /// Overwrite an existing config without asking for confirmation.
    #[arg(long)]
    pub force: bool,
    /// Print a machine-readable JSON summary of the created config.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct RegisterOptions {
    pub locale: String,
    pub model: String,
    pub name: Option<String>,
    pub jwt: Option<String>,
    pub accept_tos: bool,
}

impl From<RegisterArgs> for RegisterOptions {
    fn from(args: RegisterArgs) -> Self {
        Self {
            locale: args.locale,
            model: args.model,
            name: args.name,
            jwt: args.jwt,
            accept_tos: args.accept_tos,
        }
    }
}

impl RegisterOptions {
    /// Defaults for non-interactive `--auto-register` (TOS accepted, no JWT).
    pub fn auto_register_defaults() -> Self {
        Self {
            locale: usque_cloudflare_api::DEFAULT_LOCALE.into(),
            model: usque_cloudflare_api::DEFAULT_MODEL.into(),
            name: None,
            jwt: None,
            accept_tos: true,
        }
    }
}

/// Register and write config when no config file exists yet.
pub async fn auto_register_if_missing(opts: RegisterOptions, config_path: &str) -> Result<()> {
    if Config::load(config_path).is_ok() {
        return Ok(());
    }
    tracing::info!("No config found; registering a new client...");
    perform_register(&opts, config_path).await?;
    Ok(())
}

pub async fn perform_register(opts: &RegisterOptions, config_path: &str) -> Result<Config> {
    let client = CloudflareClient::new()?;
    let account = usque_cloudflare_api::register(
        &client,
        &opts.model,
        &opts.locale,
        opts.jwt.as_deref(),
        opts.accept_tos,
    )
    .await?;

    let (priv_key, pub_key) = generate_ec_keypair()?;
    tracing::info!("Enrolling device key...");
    let token = account.token.clone().unwrap_or_default();
    let updated = usque_cloudflare_api::enroll_key(
        &client,
        &account.id,
        &token,
        &pub_key,
        opts.name.as_deref().unwrap_or(""),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let peer = updated
        .config
        .peers
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing peer in registration response"))?;

    let cfg = Config {
        // Stored as PKCS#8 v1 (the format `ring` needs). Legacy Go configs
        // store SEC1 instead; `usque_crypto::decode_private_key` reads both.
        private_key: STANDARD.encode(priv_key),
        endpoint_v4: parse_endpoint_v4(&peer.endpoint.v4),
        endpoint_v6: parse_endpoint_v6(&peer.endpoint.v6),
        endpoint_h2_v4: DEFAULT_ENDPOINT_H2_V4.to_string(),
        endpoint_h2_v6: String::new(),
        endpoint_pub_key: peer.public_key.clone(),
        license: updated.account.license,
        id: updated.id,
        access_token: token,
        ipv4: updated.config.interface.addresses.v4,
        ipv6: updated.config.interface.addresses.v6,
    };

    cfg.save(config_path)?;
    tracing::info!("Config saved to {config_path}");
    Ok(cfg)
}

pub async fn run(args: RegisterArgs, config_path: &str) -> Result<()> {
    if !args.force && Config::load(config_path).is_ok() {
        // Never block a non-interactive caller (LuCI/uci/hotplug) on stdin.
        if !io::stdin().is_terminal() {
            bail!("config {config_path} already exists; pass --force to overwrite it");
        }
        print!("You already have a config. Do you want to overwrite it? (y/n) ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if line.trim() != "y" {
            return Ok(());
        }
    }

    let json = args.json;
    let cfg = perform_register(&args.into(), config_path).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "config": config_path,
                "id": cfg.id,
                "ipv4": cfg.ipv4,
                "ipv6": cfg.ipv6,
                "endpoint_v4": cfg.endpoint_v4,
                "endpoint_v6": cfg.endpoint_v6,
            })
        );
    }
    Ok(())
}
