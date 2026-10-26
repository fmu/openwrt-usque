mod capsule;
mod connect_ip;
mod datagram;
mod h2;
#[cfg(feature = "quiche")]
mod h3;
#[cfg(feature = "quinn")]
mod h3_quinn;
mod session;

use anyhow::Result;

pub use session::{ConnectOptions, PacketSession, SessionError};

pub use connect_ip::ConnectIpSession;
pub use datagram::decrement_ttl;
pub use h2::{connect_h2, connect_h2_on};

pub async fn connect_tunnel(options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    if options.use_http2 {
        let session = h2::connect_h2(options).await?;
        Ok(Box::new(session))
    } else {
        connect_h3(options).await
    }
}

#[cfg(feature = "quinn")]
async fn connect_h3(options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    let session = h3_quinn::connect_h3(options).await?;
    Ok(Box::new(session))
}

#[cfg(all(not(feature = "quinn"), feature = "quiche"))]
async fn connect_h3(options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    let session = h3::connect_h3(options).await?;
    Ok(Box::new(session))
}

#[cfg(all(not(feature = "quinn"), not(feature = "quiche")))]
async fn connect_h3(_options: &ConnectOptions) -> Result<Box<dyn PacketSession>> {
    anyhow::bail!("no HTTP/3 QUIC backend enabled (enable `quinn` or `quiche`)")
}
