use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};

use anyhow::{bail, Result};

use crate::Config;

pub const DEFAULT_ENDPOINT_H2_V4: &str = "162.159.198.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSelection {
    Http3V4,
    Http3V6,
    Http2V4,
    Http2V6,
}

/// Pick the MASQUE (HTTP/3, UDP/443) endpoint from a Go-compatible config.
///
/// The HTTP/2 fallback of upstream usque-rs was dropped (see
/// `usque-masque::connect_tunnel`), so `endpoint_h2_*` in the config is kept
/// only for file-format compatibility and ignored here.
pub fn select_endpoint(config: &Config, use_ipv6: bool, port: u16) -> Result<SocketAddr> {
    if use_ipv6 {
        let ip: IpAddr = config
            .endpoint_v6
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid endpoint_v6 value {:?}", config.endpoint_v6))?;
        return Ok(match ip {
            IpAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(v4, port)),
            IpAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(v6, port, 0, 0)),
        });
    }

    let ip: IpAddr = config
        .endpoint_v4
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid endpoint_v4 value {:?}", config.endpoint_v4))?;
    Ok(SocketAddr::V4(SocketAddrV4::new(
        match ip {
            IpAddr::V4(v4) => v4,
            _ => bail!("endpoint_v4 must be IPv4"),
        },
        port,
    )))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn sample_config() -> Config {
        Config {
            private_key: String::new(),
            endpoint_v4: "162.159.198.1".into(),
            endpoint_v6: "2606:4700:103::".into(),
            endpoint_h2_v4: DEFAULT_ENDPOINT_H2_V4.into(),
            endpoint_h2_v6: "2606:4700:103::".into(),
            endpoint_pub_key: String::new(),
            license: String::new(),
            id: String::new(),
            access_token: String::new(),
            ipv4: "100.96.0.1".into(),
            ipv6: "2606::1".into(),
        }
    }

    #[test]
    fn select_http3_ipv4() {
        let cfg = sample_config();
        let addr = select_endpoint(&cfg, false, 443).unwrap();
        assert_eq!(
            addr,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(162, 159, 198, 1), 443))
        );
    }

    #[test]
    fn select_http3_ipv6() {
        let cfg = sample_config();
        let addr = select_endpoint(&cfg, true, 443).unwrap();
        assert_eq!(
            addr,
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0x2606, 0x4700, 0x103, 0, 0, 0, 0, 0),
                443,
                0,
                0
            ))
        );
    }

}
