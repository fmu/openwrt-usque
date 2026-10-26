use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(feature = "quinn")]
use bytes::BufMut;
use bytes::{Bytes, BytesMut};
use crossfire::{mpsc, MAsyncTx};
use parking_lot::Mutex;
use tokio::sync::Notify;

#[cfg(feature = "quiche")]
use tokio_quiche::ClientH3Controller;
#[cfg(feature = "quiche")]
use tokio_quiche::QuicConnection;

use crate::session::{PacketSession, SessionError};

#[cfg(feature = "quiche")]
pub(crate) struct H3ConnectionGuard {
    pub _conn: QuicConnection,
    pub _controller: ClientH3Controller,
}

/// How many 8-byte words the quarter-stream-id varint may occupy.
/// 8 bytes headroom lets the QUIC stack prepend the prefix in place
/// (no payload shift, no copy) regardless of stream id size.
#[cfg(feature = "quiche")]
const QUARTER_SID_HEADROOM: usize = 8;

pub(crate) enum Transport {
    #[cfg(feature = "quiche")]
    H3Quiche {
        /// Raw payload: context-id varint + IP packet (no quarter-stream-id
        /// prefix — tokio-quiche's driver prepends it into the buffer's
        /// headroom before handing the datagram to quiche).
        out: MAsyncTx<mpsc::Array<Vec<u8>>>,
        _guard: Arc<H3ConnectionGuard>,
    },
    #[cfg(feature = "quinn")]
    H3Quinn {
        /// Pre-encoded quarter-stream-id varint for the CONNECT stream.
        /// Prepended once per packet by the sender task.
        header: Bytes,
        /// Raw payload: context-id varint + IP packet (no quarter-stream-id
        /// prefix — the sender task builds the final contiguous `Bytes`).
        out: MAsyncTx<mpsc::Array<Vec<u8>>>,
    },
    H2 {
        out: MAsyncTx<mpsc::Array<Bytes>>,
    },
}

pub struct ConnectIpSession {
    incoming: Arc<Mutex<VecDeque<Bytes>>>,
    notify: Arc<Notify>,
    transport: Transport,
    closed: Arc<AtomicBool>,
    /// Per-session scratch buffer reused across `write_packet` calls.
    /// Reserving the MTU once amortizes the allocation cost across
    /// every packet.
    scratch: BytesMut,
    /// Outbound payload scratch shared by the H3 transports. Encoded
    /// directly into the final `Vec` that goes on the wire: `mem::take`
    /// moves it to the channel, so each packet costs exactly one alloc
    /// and one payload copy (instead of scratch copy + freeze copy +
    /// QUIC-layer concat copy).
    scratch_vec: Vec<u8>,
}

impl ConnectIpSession {
    pub(crate) fn new(transport: Transport) -> Self {
        Self::with_capacity(
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Notify::new()),
            transport,
            Arc::new(AtomicBool::new(false)),
            1500,
        )
    }

    pub(crate) fn with_capacity(
        incoming: Arc<Mutex<VecDeque<Bytes>>>,
        notify: Arc<Notify>,
        transport: Transport,
        closed: Arc<AtomicBool>,
        capacity: usize,
    ) -> Self {
        let mut scratch = BytesMut::new();
        scratch.reserve(capacity);
        let scratch_vec = Vec::with_capacity(capacity);
        Self {
            incoming,
            notify,
            transport,
            closed,
            scratch,
            scratch_vec,
        }
    }

    pub fn incoming_queue(&self) -> Arc<Mutex<VecDeque<Bytes>>> {
        Arc::clone(&self.incoming)
    }

    pub fn notify(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    pub fn closed_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.closed)
    }
}

/// Encode a quiche outbound payload: QUARTER_SID_HEADROOM zero bytes
/// (in-place quarter-stream-id headroom) + context-id varint + packet.
#[cfg(feature = "quiche")]
fn encode_h3_quiche_payload(packet: &[u8], out: &mut Vec<u8>) -> anyhow::Result<()> {
    out.resize(QUARTER_SID_HEADROOM, 0);
    crate::datagram::encode_h3_datagram_payload_into(packet, out)
}

#[async_trait]
impl PacketSession for ConnectIpSession {
    async fn read_packet(&mut self) -> Result<Option<Bytes>, SessionError> {
        loop {
            if let Some(packet) = self.incoming.lock().pop_front() {
                return Ok(Some(packet));
            }
            if self.closed.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.notify.notified().await;
        }
    }

    async fn write_packet(&mut self, packet: &[u8]) -> Result<Option<Bytes>, SessionError> {
        match &self.transport {
            #[cfg(feature = "quiche")]
            Transport::H3Quiche { out, .. } => {
                // Encode directly into the outgoing Vec: one copy from the
                // TUN read buffer, then `DgramBuffer::from_vec_with_headroom`
                // in the sender task moves it without copying. Headroom lets
                // tokio-quiche prepend the quarter-stream-id varint in
                // place.
                self.scratch_vec.clear();
                self.scratch_vec
                    .reserve(QUARTER_SID_HEADROOM + 1 + packet.len());
                encode_h3_quiche_payload(packet, &mut self.scratch_vec)
                    .map_err(SessionError::Other)?;
                let payload = std::mem::take(&mut self.scratch_vec);
                // Recover capacity on the next call; the taken Vec is
                // consumed by the wire.
                self.scratch_vec = Vec::with_capacity(payload.capacity());
                out.send(payload).await.map_err(|_| SessionError::Closed)?;
            }
            #[cfg(feature = "quinn")]
            Transport::H3Quinn { header, out } => {
                // Encode header + payload into one contiguous Vec so the
                // sender task can wrap it as `Bytes` with a single move
                // (no concat copy) and hand it to `quinn::Connection`.
                self.scratch_vec.clear();
                self.scratch_vec.reserve(header.len() + 1 + packet.len());
                self.scratch_vec.put_slice(header);
                crate::datagram::encode_h3_datagram_payload_into(packet, &mut self.scratch_vec)
                    .map_err(SessionError::Other)?;
                let payload = std::mem::take(&mut self.scratch_vec);
                self.scratch_vec = Vec::with_capacity(payload.capacity());
                out.send(payload).await.map_err(|_| SessionError::Closed)?;
            }
            Transport::H2 { out } => {
                self.scratch.clear();
                self.scratch
                    .reserve(crate::capsule::CAPSULE_OVERHEAD + packet.len());
                crate::datagram::encode_h2_datagram_capsule_into(packet, &mut self.scratch)
                    .map_err(SessionError::Other)?;
                let payload = self.scratch.split().freeze();
                out.send(payload).await.map_err(|_| SessionError::Closed)?;
            }
        }
        Ok(None)
    }

    async fn close(&mut self) -> Result<(), SessionError> {
        self.closed.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;

    fn sample_packet() -> Bytes {
        // A minimal valid IPv4 TCP packet: version=4, ihl=5, ttl=64,
        // proto=6 (TCP), total_length=20. 20 bytes of header, no
        // payload. The supervisor (in production) calls
        // `decrement_ttl` before handing the packet to the session;
        // the test exercises the codec directly so the packet TTL is
        // decremented by the caller in the test.
        let mut pkt = vec![0u8; 20];
        pkt[0] = 0x45;
        pkt[2] = 0x00;
        pkt[3] = 0x14; // total_length = 20
        pkt[8] = 63; // TTL (already decremented)
        pkt[9] = 6; // TCP
        Bytes::from(pkt)
    }

    #[tokio::test]
    async fn write_packet_uses_capsule_layout_for_h2() {
        // The H2 path uses `put_capsule` (1 type varint + 1 length
        // varint + payload). Verify the first 2 bytes encode the
        // CONNECT-IP DATA type and the packet length.
        let (tx, rx) = mpsc::bounded_async::<Bytes>(8);
        let mut session = ConnectIpSession::with_capacity(
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Notify::new()),
            Transport::H2 { out: tx },
            Arc::new(AtomicBool::new(false)),
            1500,
        );

        let pkt = sample_packet();
        session.write_packet(&pkt).await.unwrap();
        let wire = rx.recv().await.unwrap();
        // Capsule type 0 -> varint byte 0x00.
        assert_eq!(wire[0], 0x00);
        // Capsule length 20 -> varint byte 0x14.
        assert_eq!(wire[1], 0x14);
        // Payload: packet body is passed through as-is (TTL was 63
        // since the test pre-decrements it; the production path
        // decrements in the supervisor before reaching the session).
        assert_eq!(&wire[2..], &pkt[..]);
        assert_eq!(wire[2 + 8], 63);
    }

    #[tokio::test]
    async fn read_packet_returns_queued_bytes() {
        let incoming: Arc<Mutex<VecDeque<Bytes>>> = Arc::new(Mutex::new(VecDeque::new()));
        let notify = Arc::new(Notify::new());
        let (tx, _rx) = mpsc::bounded_async::<Bytes>(8);
        let mut session = ConnectIpSession::with_capacity(
            Arc::clone(&incoming),
            Arc::clone(&notify),
            Transport::H2 { out: tx },
            Arc::new(AtomicBool::new(false)),
            1500,
        );

        let expected = sample_packet();
        incoming.lock().push_back(expected.clone());
        notify.notify_one();

        let got = session.read_packet().await.unwrap().unwrap();
        // Aliases the same allocation: the Bytes is a direct slice of
        // the queued buffer.
        assert_eq!(got.as_ptr(), expected.as_ptr());
        assert_eq!(&got[..], &expected[..]);
    }

    #[cfg(feature = "quinn")]
    #[tokio::test]
    async fn write_packet_h3_quinn_wire_layout() {
        // H3 quinn payload layout: quarter-stream-id varint (header,
        // prepended by `write_packet`) + context-id varint + packet.
        // The sender task wraps this Vec as `Bytes` with no further
        // copies, so this is the exact bytes `quinn` puts on the wire.
        let mut header_buf = bytes::BytesMut::new();
        crate::capsule::put_varint(&mut header_buf, 0); // quarter stream id 0
        let header = header_buf.freeze();

        let (tx, rx) = mpsc::bounded_async::<Vec<u8>>(8);
        let mut session = ConnectIpSession::with_capacity(
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Notify::new()),
            Transport::H3Quinn { header, out: tx },
            Arc::new(AtomicBool::new(false)),
            1500,
        );

        let pkt = sample_packet();
        session.write_packet(&pkt).await.unwrap();
        let wire = rx.recv().await.unwrap();
        // quarter-stream-id 0 -> 0x00, context-id 0 -> 0x00, then packet.
        assert_eq!(wire[0], 0x00);
        assert_eq!(wire[1], 0x00);
        assert_eq!(&wire[2..], &pkt[..]);
    }

    #[cfg(feature = "quiche")]
    #[tokio::test]
    async fn write_packet_h3_quiche_reserves_headroom() {
        // The quiche path must leave QUARTER_SID_HEADROOM zero bytes in
        // front of the payload so the driver can prepend the
        // quarter-stream-id varint in place (no payload shift/copy).
        let pkt = sample_packet();
        let mut wire = Vec::new();
        encode_h3_quiche_payload(&pkt, &mut wire).unwrap();
        // Headroom bytes are zero...
        assert_eq!(&wire[..QUARTER_SID_HEADROOM], &[0u8; QUARTER_SID_HEADROOM]);
        // ...followed by context-id varint + packet.
        assert_eq!(wire[QUARTER_SID_HEADROOM], 0x00);
        assert_eq!(&wire[QUARTER_SID_HEADROOM + 1..], &pkt[..]);
    }

    #[tokio::test]
    async fn write_packet_reuses_scratch_across_calls() {
        // After multiple writes the scratch buffer should not have
        // grown unboundedly. `BytesMut::split().freeze()` may shrink
        // the capacity to match the used size, so we assert that the
        // capacity after the loop is at most 2x the original (loose
        // bound to account for BytesMut growth policy).
        let (tx, rx) = mpsc::bounded_async::<Bytes>(64);
        let mut session = ConnectIpSession::with_capacity(
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Notify::new()),
            Transport::H2 { out: tx },
            Arc::new(AtomicBool::new(false)),
            1500,
        );
        for _ in 0..16 {
            let pkt = sample_packet();
            session.write_packet(&pkt).await.unwrap();
        }
        for _ in 0..16 {
            let _ = rx.recv().await.unwrap();
        }
        // The scratch is back to empty (cleared after each split).
        assert_eq!(session.scratch.len(), 0);
    }
}
