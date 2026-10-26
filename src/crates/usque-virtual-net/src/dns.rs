use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;

use crate::stack::VirtualUdpSocket;
use crate::VirtualStack;

#[derive(Clone)]
pub struct DnsResolver {
    pub servers: Vec<IpAddr>,
    pub timeout: Duration,
    pub use_os_resolver: bool,
    pub local_dns: bool,
    pub stack: Option<Arc<VirtualStack>>,
}

impl DnsResolver {
    pub async fn lookup_ips(&self, host: &str) -> anyhow::Result<Vec<IpAddr>> {
        if self.use_os_resolver {
            let lookup = tokio::net::lookup_host(format!("{host}:0")).await?;
            let mut addrs = Vec::new();
            for addr in lookup {
                push_unique_ip(&mut addrs, addr.ip());
            }
            if addrs.is_empty() {
                anyhow::bail!("no IP address for {host}");
            }
            return Ok(addrs);
        }

        if self.servers.is_empty() {
            anyhow::bail!("no DNS servers configured");
        }

        if !self.local_dns {
            if let Some(stack) = &self.stack {
                stack.wake();
                return lookup_via_tunnel(stack, &self.servers, host, self.timeout).await;
            }
        }

        lookup_via_host(&self.servers, host, self.timeout).await
    }

    pub async fn lookup_ip(&self, host: &str) -> anyhow::Result<IpAddr> {
        self.lookup_ips(host)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no IP address for {host}"))
    }
}

async fn lookup_via_tunnel(
    stack: &VirtualStack,
    servers: &[IpAddr],
    host: &str,
    timeout: Duration,
) -> anyhow::Result<Vec<IpAddr>> {
    let mut last_err = None;

    for server in servers {
        let socket = match stack.bind_udp().await {
            Ok(socket) => socket,
            Err(err) => {
                last_err = Some(err.into());
                continue;
            }
        };
        let mut ips = Vec::new();
        for qtype in [28u16, 1u16] {
            match query_dns_via_tunnel(&socket, *server, host, timeout, qtype).await {
                Ok(records) => {
                    for ip in records {
                        push_unique_ip(&mut ips, ip);
                    }
                }
                Err(err) => last_err = Some(err),
            }
        }
        if !ips.is_empty() {
            return Ok(ips);
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all DNS servers failed for {host}")))
}

async fn lookup_via_host(
    servers: &[IpAddr],
    host: &str,
    timeout: Duration,
) -> anyhow::Result<Vec<IpAddr>> {
    let mut last_err = None;

    for server in servers {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(SocketAddr::new(*server, 53)).await?;
        let mut ips = Vec::new();
        for qtype in [28u16, 1u16] {
            let query = build_dns_query(host, qtype)?;
            if tokio::time::timeout(timeout, socket.send(&query))
                .await
                .is_err()
            {
                continue;
            }
            let mut buf = [0u8; 512];
            if let Ok(Ok(n)) = tokio::time::timeout(timeout, socket.recv(&mut buf)).await {
                for ip in parse_dns_records(&buf[..n]) {
                    push_unique_ip(&mut ips, ip);
                }
            }
        }
        if !ips.is_empty() {
            return Ok(ips);
        }
        last_err = Some(anyhow::anyhow!("dns query failed for {server}"));
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all DNS servers failed for {host}")))
}

async fn query_dns_via_tunnel(
    socket: &VirtualUdpSocket,
    server: IpAddr,
    host: &str,
    timeout: Duration,
    qtype: u16,
) -> anyhow::Result<Vec<IpAddr>> {
    let remote = SocketAddr::new(server, 53);
    let query = build_dns_query(host, qtype)?;
    if tokio::time::timeout(timeout, socket.send_to(&query, remote))
        .await
        .is_err()
    {
        anyhow::bail!("dns send timed out to {server}");
    }

    let mut buf = [0u8; 512];
    match tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await {
        Ok(Ok((n, _from))) => {
            let ips = parse_dns_records(&buf[..n]);
            if ips.is_empty() {
                anyhow::bail!("no records in response from {server}");
            }
            Ok(ips)
        }
        Ok(Err(err)) => Err(err.into()),
        Err(_) => anyhow::bail!("dns recv timed out from {server}"),
    }
}

fn build_dns_query(host: &str, qtype: u16) -> anyhow::Result<Vec<u8>> {
    let mut msg = Vec::with_capacity(128);
    msg.extend_from_slice(&[0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    for label in host.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0);
    msg.extend_from_slice(&qtype.to_be_bytes());
    msg.extend_from_slice(&[0, 1]);
    Ok(msg)
}

fn parse_dns_records(buf: &[u8]) -> Vec<IpAddr> {
    if buf.len() < 12 {
        return Vec::new();
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        let Some(next) = skip_dns_name(buf, i) else {
            return Vec::new();
        };
        i = next;
        if i + 4 > buf.len() {
            return Vec::new();
        }
        i += 4;
    }

    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut ips = Vec::new();
    for _ in 0..an {
        let Some(next) = skip_dns_name(buf, i) else {
            break;
        };
        i = next;
        if i + 10 > buf.len() {
            break;
        }
        let rr_type = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let rr_class = u16::from_be_bytes([buf[i + 2], buf[i + 3]]);
        let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        i += 10;
        if i + rdlen > buf.len() {
            break;
        }
        match (rr_type, rr_class, rdlen) {
            (1, 1, 4) => {
                if let Ok(octets) = <[u8; 4]>::try_from(&buf[i..i + 4]) {
                    push_unique_ip(&mut ips, IpAddr::from(octets));
                }
            }
            (28, 1, 16) => {
                if let Ok(octets) = <[u8; 16]>::try_from(&buf[i..i + 16]) {
                    push_unique_ip(&mut ips, IpAddr::from(octets));
                }
            }
            _ => {}
        }
        i += rdlen;
    }
    ips
}

fn skip_dns_name(buf: &[u8], mut i: usize) -> Option<usize> {
    let mut steps = 0usize;
    loop {
        let len = *buf.get(i)?;
        if len & 0xc0 == 0xc0 {
            return Some(i + 2);
        }
        if len == 0 {
            return Some(i + 1);
        }
        i += 1 + len as usize;
        steps += 1;
        if steps > 128 {
            return None;
        }
    }
}

fn push_unique_ip(ips: &mut Vec<IpAddr>, ip: IpAddr) {
    if !ips.contains(&ip) {
        ips.push(ip);
    }
}
