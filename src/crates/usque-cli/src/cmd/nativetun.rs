use std::sync::Arc;

use anyhow::Result;
use clap::Args;
use usque_tun_platform::{NativeTun, NativeTunConfig};
use usque_tunnel_core::{HookEnv, MaintainTunnelConfig, TunnelSupervisor};

use crate::cmd::register::{auto_register_if_missing, RegisterOptions};
use crate::runtime::{build_connect_options, load_config, TunnelFlags};

#[derive(Debug, Args)]
pub struct NativeTunArgs {
    #[command(flatten)]
    pub tunnel: TunnelFlags,
    #[arg(short = 'I', long = "no-iproute2")]
    pub no_iproute2: bool,
    #[arg(short = 'n', long = "interface-name", default_value = "")]
    pub interface_name: String,
    #[arg(long = "persist")]
    pub persist: bool,
    /// Register a new client if the config file does not exist yet.
    #[arg(long = "auto-register")]
    pub auto_register: bool,
}

pub async fn run(args: NativeTunArgs, config_path: &str) -> Result<()> {
    if args.auto_register {
        auto_register_if_missing(RegisterOptions::auto_register_defaults(), config_path).await?;
    }
    let config = load_config(config_path)?;
    let (v4, v6) = crate::runtime::tunnel_addresses(&config, &args.tunnel);

    let tun = NativeTun::create(NativeTunConfig {
        name: args.interface_name.clone(),
        mtu: args.tunnel.mtu,
        ipv4: if v4.is_some() {
            Some(config.ipv4.clone())
        } else {
            None
        },
        ipv6: if v6.is_some() {
            Some(config.ipv6.clone())
        } else {
            None
        },
        configure_link: !args.no_iproute2,
        persist: args.persist,
    })
    .await?;

    tracing::info!("Created TUN device: {}", tun.name);

    let connect = build_connect_options(&config, &args.tunnel)?;
    let hook_env = HookEnv::default()
        .with("USQUE_MODE", "nativetun")
        .with("USQUE_IFACE", &tun.name)
        .with("USQUE_IPV4", &config.ipv4)
        .with("USQUE_IPV6", &config.ipv6);

    let maintain_cfg = MaintainTunnelConfig {
        connect,
        mtu: args.tunnel.mtu,
        reconnect_delay: args.tunnel.reconnect_delay.into(),
        always_reconnect: args.tunnel.always_reconnect,
        on_connect: args.tunnel.on_connect.clone(),
        on_disconnect: args.tunnel.on_disconnect.clone(),
        hook_env,
        activity: None,
    };

    let device = Arc::new(tun);
    TunnelSupervisor::maintain(maintain_cfg, device).await;
    Ok(())
}
