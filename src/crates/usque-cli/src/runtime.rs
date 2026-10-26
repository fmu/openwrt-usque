use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use usque_config::Config;
use usque_crypto::{
    build_rustls_config, decode_endpoint_public_key, decode_private_key, TlsOptions,
};
use usque_masque::ConnectOptions;

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
    /// Extended CONNECT `:authority` for the MASQUE endpoint (hostname or full
    /// `https://host` URI). Defaults to Cloudflare's WARP authority; override
    /// for self-hosted MASQUE/CONNECT-IP servers.
    #[arg(long = "authority", default_value = usque_cloudflare_api::CONNECT_URI)]
    pub authority: String,
    /// Extended CONNECT `:protocol` token: `cf-connect-ip` (Cloudflare's
    /// legacy WARP token, default) or `connect-ip` (RFC 9484).
    #[arg(long = "protocol", default_value = "cf-connect-ip")]
    pub protocol: String,
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

pub fn build_connect_options(config: &Config, flags: &TunnelFlags) -> Result<ConnectOptions> {
    let signing_key = decode_private_key(&config.private_key)?;
    let peer_key = decode_endpoint_public_key(&config.endpoint_pub_key)?;

    let alpn = vec![b"h3".to_vec()];

    let tls_config = build_rustls_config(
        &signing_key,
        TlsOptions {
            sni: flags.sni_address.clone(),
            insecure: flags.insecure,
            peer_public_key: peer_key.clone(),
            alpn,
        },
    )?;

    let endpoint = usque_config::select_endpoint(config, flags.ipv6, flags.connect_port)?;

    if flags.insecure {
        tracing::warn!("WARNING: --insecure is set, endpoint certificate pinning is disabled");
    }
    Ok(ConnectOptions {
        tls_config,
        insecure: flags.insecure,
        peer_public_key: peer_key,
        endpoint,
        sni: flags.sni_address.clone(),
        keepalive_period: Duration::from(flags.keepalive_period),
        initial_packet_size: flags.initial_packet_size,
        connect_uri: connect_uri_from_authority(&flags.authority),
        connect_protocol: flags.protocol.clone(),
    })
}

/// Normalise the `--authority` value into the URI that carries the extended
/// CONNECT `:authority` pseudo-header.
///
/// Accepts either a bare host (`masque.example.com`) or a full URI
/// (`https://masque.example.com`), because the UCI option is documented as an
/// authority/host but operators may paste a URL.
pub fn connect_uri_from_authority(authority: &str) -> String {
    let authority = authority.trim();
    if authority.contains("://") {
        authority.to_string()
    } else if authority.is_empty() {
        usque_cloudflare_api::CONNECT_URI.to_string()
    } else {
        format!("https://{authority}")
    }
}

#[cfg(test)]
mod tests {
    use super::connect_uri_from_authority;
    use usque_cloudflare_api::CONNECT_URI;

    #[test]
    fn default_authority_matches_cloudflare() {
        assert_eq!(connect_uri_from_authority(CONNECT_URI), CONNECT_URI);
    }

    #[test]
    fn bare_host_becomes_https_uri() {
        assert_eq!(
            connect_uri_from_authority("masque.example.com"),
            "https://masque.example.com"
        );
        assert_eq!(
            connect_uri_from_authority(" masque.example.com:8443 "),
            "https://masque.example.com:8443"
        );
    }

    #[test]
    fn full_uri_is_passed_through() {
        assert_eq!(
            connect_uri_from_authority("https://masque.example.com/connect"),
            "https://masque.example.com/connect"
        );
    }

    #[test]
    fn empty_authority_falls_back_to_default() {
        assert_eq!(connect_uri_from_authority(""), CONNECT_URI);
    }
}
