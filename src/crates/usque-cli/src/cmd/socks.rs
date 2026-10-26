use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Args;

use usque_proxy_socks::{run as run_socks, SocksProxyConfig};
use usque_virtual_net::dns::DnsResolver;

use crate::cmd::register::{auto_register_if_missing, RegisterOptions};
use crate::runtime::{load_config, spawn_userspace_tunnel, TunnelFlags};

#[derive(Debug, Args)]
pub struct SocksArgs {
    #[command(flatten)]
    pub tunnel: TunnelFlags,
    /// Register a new client when config is missing, then start the proxy
    #[arg(long = "auto-register")]
    pub auto_register: bool,
    #[arg(short = 'b', long, default_value = "0.0.0.0")]
    pub bind: String,
    #[arg(short = 'p', long, default_value = "1080")]
    pub port: u16,
    #[arg(short = 'u', long)]
    pub username: Option<String>,
    #[arg(short = 'w', long)]
    pub password: Option<String>,
    #[arg(short = 'd', long = "dns", value_delimiter = ',')]
    pub dns: Vec<String>,
    #[arg(short = 't', long = "dns-timeout", default_value = "2s")]
    pub dns_timeout: humantime::Duration,
    #[arg(short = 'l', long = "local-dns")]
    pub local_dns: bool,
    #[arg(long = "system-dns")]
    pub system_dns: bool,
    #[arg(long = "udp-timeout", default_value = "60s")]
    pub udp_timeout: humantime::Duration,
}

pub async fn run(args: SocksArgs, config_path: &str) -> Result<()> {
    if args.auto_register {
        auto_register_if_missing(RegisterOptions::auto_register_defaults(), config_path).await?;
    }

    let config = load_config(config_path)?;
    let (stack, _device, _handle) = spawn_userspace_tunnel(&config, &args.tunnel, "socks")?;

    let servers = if args.dns.is_empty() {
        vec![
            "9.9.9.9".parse().unwrap(),
            "149.112.112.112".parse().unwrap(),
            "2620:fe::fe".parse().unwrap(),
            "2620:fe::9".parse().unwrap(),
        ]
    } else {
        args.dns.iter().map(|s| s.parse().unwrap()).collect()
    };

    let resolver = DnsResolver {
        servers,
        timeout: Duration::from(args.dns_timeout),
        use_os_resolver: args.local_dns && args.system_dns,
        local_dns: args.local_dns,
        stack: if args.local_dns {
            None
        } else {
            Some(Arc::clone(&stack))
        },
    };

    let bind: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    run_socks(
        SocksProxyConfig {
            bind,
            username: args.username,
            password: args.password,
            resolver,
            udp_timeout: Duration::from(args.udp_timeout),
        },
        stack,
    )
    .await
}
