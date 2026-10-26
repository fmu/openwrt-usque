use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use usque_virtual_net::VirtualStack;

#[derive(Debug, Clone)]
pub struct PortMapping {
    pub bind_address: String,
    pub local_port: u16,
    pub remote_ip: String,
    pub remote_port: u16,
}

pub fn parse_port_mapping(input: &str) -> Result<PortMapping> {
    let parts: Vec<&str> = input.split(':').collect();
    let (bind, local_port, remote_ip, remote_port) = match parts.len() {
        3 => ("localhost", parts[0], parts[1], parts[2]),
        4 => (parts[0], parts[1], parts[2], parts[3]),
        _ => anyhow::bail!("invalid port mapping format"),
    };

    Ok(PortMapping {
        bind_address: bind.to_string(),
        local_port: local_port.parse().context("invalid local port")?,
        remote_ip: remote_ip.to_string(),
        remote_port: remote_port.parse().context("invalid remote port")?,
    })
}

pub async fn run_local_forwards(mappings: Vec<PortMapping>, stack: std::sync::Arc<VirtualStack>) {
    for mapping in mappings {
        let stack = std::sync::Arc::clone(&stack);
        tokio::spawn(async move {
            if let Err(err) = forward_local(mapping, stack).await {
                tracing::warn!("local forward error: {err}");
            }
        });
    }
}

pub async fn run_remote_forwards(mappings: Vec<PortMapping>, stack: std::sync::Arc<VirtualStack>) {
    for mapping in mappings {
        let stack = std::sync::Arc::clone(&stack);
        tokio::spawn(async move {
            if let Err(err) = forward_remote(mapping, stack).await {
                tracing::warn!("remote forward error: {err}");
            }
        });
    }
}

async fn forward_local(mapping: PortMapping, stack: std::sync::Arc<VirtualStack>) -> Result<()> {
    let bind: SocketAddr = format!("{}:{}", mapping.bind_address, mapping.local_port)
        .parse()
        .context("invalid bind address")?;
    let listener = TcpListener::bind(bind).await?;
    info!(
        "Local forwarding: listening on {bind}, forwarding to {}:{}",
        mapping.remote_ip, mapping.remote_port
    );

    let remote: SocketAddr = format!("{}:{}", mapping.remote_ip, mapping.remote_port).parse()?;
    loop {
        let (client, _) = listener.accept().await?;
        let stack = std::sync::Arc::clone(&stack);
        tokio::spawn(async move {
            if let Ok(remote_conn) = stack.dial_tcp(remote).await {
                let _ = relay(client, remote_conn).await;
            }
        });
    }
}

async fn forward_remote(mapping: PortMapping, stack: std::sync::Arc<VirtualStack>) -> Result<()> {
    let bind: SocketAddr = format!("{}:{}", mapping.bind_address, mapping.local_port)
        .parse()
        .context("invalid bind address")?;
    let listener = stack.listen_tcp(bind).await?;
    info!(
        "Remote forwarding: listening on MASQUE network {bind}, forwarding to {}:{}",
        mapping.remote_ip, mapping.remote_port
    );

    let remote: SocketAddr = format!("{}:{}", mapping.remote_ip, mapping.remote_port).parse()?;
    loop {
        let (client, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Ok(remote_conn) = TcpStream::connect(remote).await {
                let _ = relay(client, remote_conn).await;
            }
        });
    }
}

async fn relay<A, B>(mut a: A, mut b: B) -> Result<()>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
    Ok(())
}

pub fn kick_tunnel(stack: &VirtualStack) {
    stack.wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_three_part_mapping() {
        let m = parse_port_mapping("8080:127.0.0.1:80").unwrap();
        assert_eq!(m.bind_address, "localhost");
        assert_eq!(m.local_port, 8080);
        assert_eq!(m.remote_ip, "127.0.0.1");
        assert_eq!(m.remote_port, 80);
    }

    #[test]
    fn parse_four_part_mapping() {
        let m = parse_port_mapping("0.0.0.0:9090:example.com:443").unwrap();
        assert_eq!(m.bind_address, "0.0.0.0");
        assert_eq!(m.local_port, 9090);
        assert_eq!(m.remote_ip, "example.com");
        assert_eq!(m.remote_port, 443);
    }

    #[test]
    fn reject_invalid_mapping() {
        assert!(parse_port_mapping("bad").is_err());
    }
}
