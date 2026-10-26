use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fast_socks5::server::Socks5ServerProtocol;
use fast_socks5::util::target_addr::TargetAddr;
use fast_socks5::Socks5Command;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, UdpSocket};
use tracing::info;

use usque_virtual_net::dns::DnsResolver;
use usque_virtual_net::VirtualStack;

pub struct SocksProxyConfig {
    pub bind: SocketAddr,
    pub username: Option<String>,
    pub password: Option<String>,
    pub resolver: DnsResolver,
    pub udp_timeout: Duration,
}

pub async fn run(cfg: SocksProxyConfig, stack: Arc<VirtualStack>) -> Result<()> {
    let listener = TcpListener::bind(cfg.bind).await?;
    info!("SOCKS5 proxy listening on {}", cfg.bind);

    loop {
        let (socket, _) = listener.accept().await?;
        let resolver = cfg.resolver.clone();
        let stack = Arc::clone(&stack);
        let username = cfg.username.clone();
        let password = cfg.password.clone();
        let udp_timeout = cfg.udp_timeout;

        tokio::spawn(async move {
            if let Err(err) =
                serve_connection(socket, username, password, resolver, udp_timeout, stack).await
            {
                tracing::debug!("socks connection failed: {err}");
            }
        });
    }
}

async fn serve_connection(
    socket: tokio::net::TcpStream,
    username: Option<String>,
    password: Option<String>,
    resolver: DnsResolver,
    udp_timeout: Duration,
    stack: Arc<VirtualStack>,
) -> Result<()> {
    let (proto, cmd, target_addr) = match (username, password) {
        (Some(username), Some(password)) => {
            Socks5ServerProtocol::accept_password_auth(socket, |user, pass| {
                user == username && pass == password
            })
            .await?
            .0
            .read_command()
            .await?
        }
        _ => {
            Socks5ServerProtocol::accept_no_auth(socket)
                .await?
                .read_command()
                .await?
        }
    };

    match cmd {
        Socks5Command::TCPConnect => {
            let addr = resolve_target(&resolver, target_addr).await?;
            let mut remote = stack.dial_tcp(addr).await?;
            let mut client = proto.reply_success(success_bind_addr()).await?;
            let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
        }
        Socks5Command::UDPAssociate => {
            let udp = UdpSocket::bind("[::]:0").await?;
            let bind = udp
                .local_addr()
                .unwrap_or_else(|_| SocketAddr::from((IpAddr::V4(Ipv4Addr::LOCALHOST), 0)));
            let client = proto.reply_success(bind).await?;
            let _ = relay_udp(client, udp, stack, udp_timeout).await;
        }
        Socks5Command::TCPBind => {}
    }

    Ok(())
}

fn success_bind_addr() -> SocketAddr {
    SocketAddr::from((IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
}

async fn relay_udp(
    mut client: tokio::net::TcpStream,
    udp: UdpSocket,
    _stack: Arc<VirtualStack>,
    _timeout: Duration,
) -> std::io::Result<()> {
    let mut tcp_buf = vec![0u8; 4096];
    let mut udp_buf = vec![0u8; 65507];
    loop {
        tokio::select! {
            read = client.read(&mut tcp_buf) => {
                if read.unwrap_or(0) == 0 {
                    break;
                }
            }
            recv = udp.recv_from(&mut udp_buf) => {
                match recv {
                    Ok((n, _peer)) if n > 0 => {}
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

async fn resolve_target(resolver: &DnsResolver, target: TargetAddr) -> Result<SocketAddr> {
    match target {
        TargetAddr::Ip(addr) => Ok(addr),
        TargetAddr::Domain(domain, port) => {
            let ips = resolver.lookup_ips(&domain).await?;
            let ip = ips
                .iter()
                .find(|addr| addr.is_ipv6())
                .or_else(|| ips.first())
                .copied()
                .ok_or_else(|| anyhow::anyhow!("no IP address for {domain}"))?;
            Ok(SocketAddr::new(ip, port))
        }
    }
}
