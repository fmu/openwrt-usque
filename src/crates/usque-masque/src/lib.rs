mod capsule;
mod connect_ip;
mod datagram;
#[cfg(feature = "quinn")]
mod h3_quinn;
mod session;

use anyhow::Result;

pub use session::{ConnectOptions, PacketSession, SessionError};

pub use connect_ip::ConnectIpSession;
pub use datagram::decrement_ttl;

/// Open a MASQUE CONNECT-IP session over HTTP/3 (QUIC, UDP/443).
///
/// The HTTP/2 (TCP/443) fallback of upstream usque-rs was removed: it pulls in
/// `h2` + the `hyper`/`tower` HTTP client stack, which the OpenWrt overlay
/// cannot afford, and the target networks (including the tested iPhone
/// tethering path) pass UDP/443.
pub async fn connect_tunnel(options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    connect_h3(options).await
}

#[cfg(feature = "quinn")]
async fn connect_h3(options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    let session = h3_quinn::connect_h3(options).await?;
    Ok(Box::new(session))
}

#[cfg(not(feature = "quinn"))]
async fn connect_h3(_options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    anyhow::bail!("no HTTP/3 QUIC backend enabled (enable the `quinn` feature)")
}
