use std::io;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};

/// Abstraction over native TUN and userspace packet devices.
///
/// `read_packet` writes into a caller-supplied `BytesMut` and returns
/// the number of bytes written. The caller can `freeze()` the buffer
/// and pass the resulting `Bytes` to the datagram writer without
/// re-copying.
///
/// `write_packet` takes its `Bytes` by value so the
/// `ChannelDevice` (and any other `mpsc`-backed implementation) can
/// move the buffer into the outbound queue without re-copying. The
/// kernel TUN implementation only needs a `&[u8]` view, so it borrows
/// from the `Bytes` and drops the buffer at the end of the call.
///
/// The supervisor wraps the device in `Arc<D>` (no external `Mutex`).
/// It never holds any lock across an `await` inside its bidirectional
/// `select!` loop; that pattern deadlocks whenever the other arm also
/// needs the same lock.
#[async_trait]
pub trait TunnelDevice: Send + Sync {
    async fn read_packet(&self, buf: &mut BytesMut) -> io::Result<usize>;
    async fn write_packet(&self, packet: Bytes) -> io::Result<()>;
}
