use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Args;
use tokio::sync::Notify;
use usque_config::Config;
use usque_tunnel_core::{HookEnv, MaintainTunnelConfig, TunnelSupervisor};
use usque_virtual_net::{ChannelDevice, VirtualStack};

use crate::runtime::tunnel::build_connect_options;

/// Default tunnel MTU: the largest value empirically measured to fit
/// inside a single QUIC DATAGRAM against Cloudflare WARP. The QUIC
/// stack (quinn) negotiates a path MTU of up to 1452 (its MTU-discovery
/// ceiling); subtracting the QUIC short header (~25B), AEAD tag (16B)
/// and DATAGRAM framing (quarter-stream-id + context-id, ~5B) leaves
/// ~1406B of IP payload. 1400 is that ceiling rounded down; anything
/// above it is silently dropped by the peer (`TooLarge` per packet).
/// IPv6's minimum mandatory MTU is 1280, so 1400 keeps every modern
/// path happy while carrying ~9% larger frames than the old 1280
/// default ported from the Go client.
pub const DEFAULT_TUNNEL_MTU: usize = 1400;

#[derive(Debug, Args, Clone)]
pub struct TunnelFlags {
    #[arg(short = 'P', long, default_value_t = 443)]
    pub connect_port: u16,
    #[arg(short = '6', long)]
    pub ipv6: bool,
    #[arg(short = 'F', long = "no-tunnel-ipv4")]
    pub no_tunnel_ipv4: bool,
    #[arg(short = 'S', long = "no-tunnel-ipv6")]
    pub no_tunnel_ipv6: bool,
    #[arg(short = 's', long = "sni-address", default_value = usque_cloudflare_api::CONNECT_SNI)]
    pub sni_address: String,
    #[arg(short = 'k', long = "keepalive-period", default_value = "30s")]
    pub keepalive_period: humantime::Duration,
    #[arg(short = 'm', long = "mtu", default_value_t = DEFAULT_TUNNEL_MTU)]
    pub mtu: usize,
    #[arg(short = 'i', long = "initial-packet-size", default_value_t = 0)]
    pub initial_packet_size: u16,
    #[arg(short = 'r', long = "reconnect-delay", default_value = "1s")]
    pub reconnect_delay: humantime::Duration,
    #[arg(long = "always-reconnect")]
    pub always_reconnect: bool,
    #[arg(long = "http2")]
    pub http2: bool,
    #[arg(long = "insecure")]
    pub insecure: bool,
    #[arg(long = "on-connect", default_value = "")]
    pub on_connect: String,
    #[arg(long = "on-disconnect", default_value = "")]
    pub on_disconnect: String,
}

pub fn load_config(path: &str) -> Result<Config> {
    Config::load(path).with_context(|| format!("config file not found at {path}"))
}

pub type UserspaceTunnelHandle = tokio::task::JoinHandle<()>;
pub type UserspaceTunnelSpawn = (Arc<VirtualStack>, Arc<ChannelDevice>, UserspaceTunnelHandle);

pub fn tunnel_addresses(cfg: &Config, flags: &TunnelFlags) -> (Option<IpAddr>, Option<IpAddr>) {
    let v4 = if flags.no_tunnel_ipv4 {
        None
    } else {
        Some(cfg.ipv4.parse().expect("invalid ipv4 in config"))
    };
    let v6 = if flags.no_tunnel_ipv6 {
        None
    } else {
        Some(cfg.ipv6.parse().expect("invalid ipv6 in config"))
    };
    (v4, v6)
}

pub fn spawn_userspace_tunnel(
    config: &Config,
    flags: &TunnelFlags,
    mode: &str,
) -> Result<UserspaceTunnelSpawn> {
    let (v4, v6) = tunnel_addresses(config, flags);
    let (device, to_device, from_device) = ChannelDevice::pair();
    let device = Arc::new(device);

    let activity = Arc::new(Notify::new());
    let stack = Arc::new(VirtualStack::start(
        v4,
        v6,
        flags.mtu,
        from_device,
        to_device,
        Arc::clone(&activity),
    ));

    let connect = build_connect_options(config, flags)?;
    let hook_env = HookEnv::default()
        .with("USQUE_MODE", mode)
        .with("USQUE_IPV4", &config.ipv4)
        .with("USQUE_IPV6", &config.ipv6);

    let maintain_cfg = MaintainTunnelConfig {
        connect,
        mtu: flags.mtu,
        reconnect_delay: flags.reconnect_delay.into(),
        always_reconnect: flags.always_reconnect,
        on_connect: flags.on_connect.clone(),
        on_disconnect: flags.on_disconnect.clone(),
        hook_env,
        activity: Some(activity),
    };

    let device_for_tunnel = Arc::clone(&device);
    let handle = tokio::spawn(async move {
        TunnelSupervisor::maintain(maintain_cfg, device_for_tunnel).await;
    });

    Ok((stack, device, handle))
}
