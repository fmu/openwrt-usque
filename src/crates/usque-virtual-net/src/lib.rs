use std::io;
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use crossfire::{mpsc, AsyncRx, MTx};
use tokio::sync::Mutex;
use usque_tunnel_core::TunnelDevice;

pub mod dns;
mod stack;

pub use stack::{VirtualStack, VirtualTcpStream};

/// Packet channel backing [`ChannelDevice`]: an unbounded crossfire
/// MPSC with a *blocking* sender (the smoltcp `TxToken::consume` path
/// is synchronous) and an async receiver (the device read path).
///
/// Mirrors the old `tokio::sync::mpsc::unbounded_channel` shape:
/// `send()` is synchronous and never blocks for an unbounded channel.
pub type PacketTx = MTx<mpsc::List<Bytes>>;
/// Async-receiver half of the packet channel; see [`PacketTx`].
pub type PacketRx = AsyncRx<mpsc::List<Bytes>>;

/// Channel-backed packet device used by userspace modes.
pub struct ChannelDevice {
    inbound: Mutex<PacketRx>,
    outbound: PacketTx,
}

impl ChannelDevice {
    pub fn pair() -> (Self, PacketTx, PacketRx) {
        let (to_tunnel_tx, to_tunnel_rx) = mpsc::unbounded_async::<Bytes>();
        let (from_tunnel_tx, from_tunnel_rx) = mpsc::unbounded_async::<Bytes>();
        (
            Self {
                inbound: Mutex::new(to_tunnel_rx),
                outbound: from_tunnel_tx,
            },
            to_tunnel_tx,
            from_tunnel_rx,
        )
    }
}

#[async_trait::async_trait]
impl TunnelDevice for ChannelDevice {
    async fn read_packet(&self, buf: &mut BytesMut) -> io::Result<usize> {
        // `crossfire`'s single-consumer `AsyncRx` is `!Sync`, so it is
        // wrapped in an async `Mutex` to make `ChannelDevice: Sync`
        // while keeping the trait's `&self` API (`recv()` itself only
        // borrows `&self`). Locking is held only for the duration of
        // `recv()`; the supervisor's other arm does not contend on
        // this lock (it writes to the device, not reads), so the
        // bidirectional `select!` cannot deadlock.
        let packet = {
            let rx = self.inbound.lock().await;
            match rx.recv().await {
                Ok(p) => p,
                // All senders dropped: the stack side is gone.
                Err(_) => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        };
        // The supervisor hands us a scratch buffer whose `len` is 0 (it
        // called `clear()` before the read). We must write into the
        // buffer's spare capacity, not the zero-length logical prefix.
        let spare = buf.spare_capacity_mut();
        let n = packet.len().min(spare.len());
        // SAFETY: `spare` exposes `buf.capacity() - buf.len()` bytes of
        // allocated-but-uninitialized memory at the tail of the buffer.
        // The caller (supervisor) reset `len` to 0 before calling, so
        // `spare` covers the full allocation. We copy `n` bytes and
        // commit with `set_len` below; the supervisor will `split_to(n)`
        // to detach the written prefix.
        let dst = unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast::<u8>(), n) };
        dst.copy_from_slice(&packet[..n]);
        unsafe {
            buf.set_len(n);
        }
        Ok(n)
    }

    async fn write_packet(&self, packet: Bytes) -> io::Result<()> {
        // The supervisor hands us a `Bytes` it owns; we move it into
        // the outbound queue without re-copying. This is the
        // zero-copy inbound path: the supervisor's `Bytes` aliases
        // the QUIC receive buffer (or the per-session scratch on the
        // ICMP path), we move the refcount up by one, and the
        // smoltcp poll task eventually hands it to the TxToken.
        self.outbound
            .send(packet)
            .map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
}

pub type SharedVirtualStack = Arc<VirtualStack>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::BytesMut;
    use tokio::sync::Notify;

    use super::*;

    #[tokio::test]
    async fn stack_keeps_tunnel_channel_open() {
        let (device, to_tunnel, from_tunnel) = ChannelDevice::pair();
        let activity = Arc::new(Notify::new());
        let _stack = VirtualStack::start(None, None, 1280, from_tunnel, to_tunnel, activity);

        let mut buf = BytesMut::with_capacity(1280);
        let read = tokio::spawn(async move { device.read_packet(&mut buf).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !read.is_finished(),
            "read should block while channel is open"
        );
        read.abort();
    }
}
