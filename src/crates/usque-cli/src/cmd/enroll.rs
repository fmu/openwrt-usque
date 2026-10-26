use std::io::{self, Write};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Args;
use usque_cloudflare_api::{self, CloudflareClient, INVALID_PUBLIC_KEY};
use usque_config::{parse_endpoint_v4, parse_endpoint_v6, Config, DEFAULT_ENDPOINT_H2_V4};
use usque_crypto::{
    decode_private_key, generate_ec_keypair, private_key_pkcs8_der, public_key_spki_der,
};

use crate::runtime::load_config;

#[derive(Debug, Args)]
pub struct EnrollArgs {
    #[arg(short = 'n', long)]
    pub name: Option<String>,
    #[arg(short = 'r', long = "regen-key")]
    pub regen_key: bool,
}

pub async fn run(args: EnrollArgs, config_path: &str) -> Result<()> {
    let existing = load_config(config_path)?;
    tracing::info!("Enrolling device key...");

    let (mut priv_bytes, mut pub_bytes) = if args.regen_key {
        tracing::info!("Regenerating key pair...");
        generate_ec_keypair()?
    } else {
        let key = decode_private_key(&existing.private_key)?;
        (
            private_key_pkcs8_der(&key)?,
            public_key_spki_der(&key)?,
        )
    };

    let client = CloudflareClient::new();
    let updated = match usque_cloudflare_api::enroll_key(
        &client,
        &existing.id,
        &existing.access_token,
        &pub_bytes,
        args.name.as_deref().unwrap_or(""),
    )
    .await
    {
        Ok(data) => data,
        Err(err) if err.body.has_error_message(INVALID_PUBLIC_KEY) => {
            print!("Invalid public key detected. Regenerate key? (y/n): ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            if line.trim() != "y" {
                anyhow::bail!("enrollment aborted");
            }
            let (new_priv, new_pub) = generate_ec_keypair()?;
            priv_bytes = new_priv;
            pub_bytes = new_pub;
            usque_cloudflare_api::enroll_key(
                &client,
                &existing.id,
                &existing.access_token,
                &pub_bytes,
                args.name.as_deref().unwrap_or(""),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
        }
        Err(err) => return Err(anyhow::anyhow!("{err}")),
    };

    let peer = updated
        .config
        .peers
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing peer in enroll response"))?;

    let h2v4 = if existing.endpoint_h2_v4.is_empty() {
        DEFAULT_ENDPOINT_H2_V4.to_string()
    } else {
        existing.endpoint_h2_v4.clone()
    };

    let cfg = Config {
        private_key: STANDARD.encode(priv_bytes),
        endpoint_v4: parse_endpoint_v4(&peer.endpoint.v4),
        endpoint_v6: parse_endpoint_v6(&peer.endpoint.v6),
        endpoint_h2_v4: h2v4,
        endpoint_h2_v6: existing.endpoint_h2_v6,
        endpoint_pub_key: peer.public_key.clone(),
        license: updated.account.license,
        id: updated.id,
        access_token: existing.access_token,
        ipv4: updated.config.interface.addresses.v4,
        ipv6: updated.config.interface.addresses.v6,
    };

    cfg.save(config_path)?;
    tracing::info!("Config saved to {config_path}");
    Ok(())
}
