use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant as StdInstant};

use bytes::{Bytes, BytesMut};
use etherparse::{NetHeaders, PacketHeaders, PayloadSlice, TransportHeader};
use parking_lot::Mutex;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, Medium};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv6Address,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::futures::OwnedNotified;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::{PacketRx, PacketTx};

struct StackClock {
    start: StdInstant,
}

impl StackClock {
    fn new() -> Self {
        Self {
            start: StdInstant::now(),
        }
    }

    fn now(&self) -> Instant {
        Instant::from_millis(self.start.elapsed().as_millis() as i64)
    }
}

struct ChannelPhy {
    rx: VecDeque<Bytes>,
    tx: PacketTx,
    /// MTU reported to smoltcp. Must match the tunnel MTU so the
    /// userspace stack never emits a frame the QUIC datagram path
    /// cannot carry.
    mtu: usize,
    recent_syns: Arc<Mutex<VecDeque<SynFingerprint>>>,
    /// Reusable transmit scratch buffer. The smoltcp driver creates
    /// a new `TxToken` for every transmit but only one is alive at a
    /// time; this means the scratch buffer is safe to share. Holding
    /// it in an `Arc<parking_lot::Mutex<_>>` inside the `TxToken` lets
    /// us *swap* it for a fresh `BytesMut` after each `consume()`
    /// while still letting the device live inside the
    /// `Send + Sync` `StackShared`.
    ///
    /// The scratch is recreated after every `consume()` rather than
    /// reused: `BytesMut::split_to(len).freeze()` advances the
    /// underlying `Arc<Vec<u8>>` pointer by `len` and `clear()` does
    /// not move it back, so a long-lived scratch would shrink its
    /// apparent capacity to zero after enough transmits (same class
    /// of bug as `TunnelSupervisor::run_pumps`'s `to_tunnel`).
    tx_scratch: Arc<Mutex<BytesMut>>,
}

impl ChannelPhy {
    fn new(tx: PacketTx, mtu: usize) -> Self {
        Self {
            rx: VecDeque::new(),
            tx,
            mtu,
            recent_syns: Arc::new(Mutex::new(VecDeque::with_capacity(16))),
            tx_scratch: Arc::new(Mutex::new(BytesMut::new())),
        }
    }

    fn push_rx(&mut self, packet: Bytes) {
        // The RST-ACK normalization step needs an in-place mutation
        // of the buffer. We can't get a `&mut [u8]` from a `Bytes`
        // (the API is immutable), so we copy. The packet is small
        // (≤ MTU bytes) and this is one allocation per inbound
        // packet; the larger copy (DgramBuffer → Bytes) happens
        // upstream in the H3 receiver and is unavoidable with
        // datagram-socket 0.8. Future work: change the channel to
        // carry `BytesMut` directly so the copy can be elided.
        let mut owned = packet.to_vec();
        self.normalize_inbound_rst_ack(&mut owned);
        self.rx.push_back(Bytes::from(owned));
    }

    fn normalize_inbound_rst_ack(&self, packet: &mut [u8]) {
        let Ok(mut headers) = PacketHeaders::from_ip_slice(packet) else {
            return;
        };
        let tcp = match &mut headers.transport {
            Some(TransportHeader::Tcp(header)) => header,
            _ => return,
        };
        if !(tcp.rst
            && tcp.ack
            && !tcp.syn
            && !tcp.fin
            && !tcp.psh
            && !tcp.urg
            && !tcp.ece
            && !tcp.cwr
            && !tcp.ns)
        {
            return;
        }

        let (local_ip, remote_ip) = match &headers.net {
            Some(NetHeaders::Ipv4(ipv4, _)) => (
                IpAddr::V4(ipv4.destination.into()),
                IpAddr::V4(ipv4.source.into()),
            ),
            Some(NetHeaders::Ipv6(ipv6, _)) => (
                IpAddr::V6(ipv6.destination.into()),
                IpAddr::V6(ipv6.source.into()),
            ),
            Some(NetHeaders::Arp(_)) | None => return,
        };

        let mut recent_syns = self.recent_syns.lock();
        let Some((idx, _syn)) = recent_syns.iter().enumerate().find(|(_, syn)| {
            syn.src == local_ip
                && syn.dst == remote_ip
                && syn.sport == tcp.destination_port
                && syn.dport == tcp.source_port
                && syn.seq == tcp.acknowledgment_number
        }) else {
            return;
        };

        tcp.acknowledgment_number = tcp.acknowledgment_number.wrapping_add(1);
        let payload = match headers.payload {
            PayloadSlice::Tcp(payload) => payload,
            _ => return,
        };

        let ip_header_len = match &mut headers.net {
            Some(NetHeaders::Ipv4(ipv4, _)) => {
                tcp.checksum = tcp.calc_checksum_ipv4(ipv4, payload).unwrap_or_default();
                ipv4.header_len()
            }
            Some(NetHeaders::Ipv6(ipv6, _)) => {
                tcp.checksum = tcp.calc_checksum_ipv6(ipv6, payload).unwrap_or_default();
                ipv6.header_len()
            }
            Some(NetHeaders::Arp(_)) | None => return,
        };

        let tcp_header_len = tcp.header_len();
        if packet.len() < ip_header_len + tcp_header_len {
            return;
        }

        match &mut headers.net {
            Some(NetHeaders::Ipv4(ipv4, _)) => {
                let mut ip_cursor = std::io::Cursor::new(&mut packet[..ip_header_len]);
                if ipv4.write(&mut ip_cursor).is_err() {
                    return;
                }
            }
            Some(NetHeaders::Ipv6(ipv6, _)) => {
                let mut ip_cursor = std::io::Cursor::new(&mut packet[..ip_header_len]);
                if ipv6.write(&mut ip_cursor).is_err() {
                    return;
                }
            }
            Some(NetHeaders::Arp(_)) | None => return,
        }

        let mut tcp_cursor =
            std::io::Cursor::new(&mut packet[ip_header_len..ip_header_len + tcp_header_len]);
        if tcp.write(&mut tcp_cursor).is_err() {
            return;
        }
        recent_syns.remove(idx);
    }
}

struct RxToken {
    buffer: Bytes,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer)
    }
}

struct TxToken {
    scratch: Arc<Mutex<BytesMut>>,
    tx: PacketTx,
    recent_syns: Arc<Mutex<VecDeque<SynFingerprint>>>,
}

// SAFETY: smoltcp's driver creates at most one TxToken at a time and
// drops it before the next `transmit()` call, so the scratch mutex
// is only ever contended by the borrow inside `consume` itself.
unsafe impl Send for TxToken {}
unsafe impl Sync for TxToken {}

impl phy::TxToken for TxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let result = {
            let mut scratch = self.scratch.lock();
            scratch.clear();
            if scratch.capacity() < len {
                scratch.reserve(len);
            }
            // `BytesMut::Deref` is `&[u8]` whose length is `len`, not
            // `capacity`. To get a `&mut [u8]` of `len` bytes we have to
            // use `spare_capacity_mut` (which exposes the tail of the
            // allocation up to `capacity - len`) and then commit it with
            // `set_len` after the closure returns.
            let spare = scratch.spare_capacity_mut();
            // Uninitialized memory is fine to expose to the closure
            // because smoltcp's contract is "fill it with a packet of
            // exactly `len` bytes".
            let slice: &mut [u8] =
                unsafe { std::slice::from_raw_parts_mut(spare.as_mut_ptr().cast::<u8>(), len) };
            let result = f(slice);
            // Commit the bytes the closure wrote.
            unsafe {
                scratch.set_len(len);
            }
            result
        };

        // Detach the just-written prefix into a `Bytes` for the
        // channel, then *swap the scratch* for a fresh one. We can't
        // just `clear()` and reuse: `BytesMut::split_to(len).freeze()`
        // advances the underlying `Arc<Vec<u8>>` pointer by `len`, so
        // a long-lived scratch would shrink to zero capacity and
        // smoltcp would start handing us zero-length transmit slices
        // (same class of bug as the supervisor's `to_tunnel`).
        //
        // Sizing: the new scratch starts at `max(prev_cap, len)`, so
        // the allocation grows monotonically to the largest packet
        // we have ever seen and then stops. The old `BytesMut` (whose
        // remaining tail capacity is `prev_cap - len` after the
        // `split_to`) is dropped here; the underlying `Vec<u8>` is
        // freed unless the frozen `Bytes` still holds a reference.
        let (used, new_cap) = {
            let mut scratch = self.scratch.lock();
            let prev_cap = scratch.capacity();
            let used = scratch.split_to(len).freeze();
            (used, prev_cap.max(len))
        };
        *self.scratch.lock() = BytesMut::with_capacity(new_cap);

        // Record SYN fingerprints for outbound SYNs.
        if let Ok(headers) = PacketHeaders::from_ip_slice(&used) {
            if let (Some(net), Some(TransportHeader::Tcp(tcp))) = (headers.net, headers.transport) {
                if tcp.syn && !tcp.ack && !tcp.rst {
                    let (src, dst) = match net {
                        NetHeaders::Ipv4(ipv4, _) => (
                            IpAddr::V4(ipv4.source.into()),
                            IpAddr::V4(ipv4.destination.into()),
                        ),
                        NetHeaders::Ipv6(ipv6, _) => (
                            IpAddr::V6(ipv6.source.into()),
                            IpAddr::V6(ipv6.destination.into()),
                        ),
                        NetHeaders::Arp(_) => return result,
                    };
                    let mut recent_syns = self.recent_syns.lock();
                    if recent_syns.len() >= 16 {
                        recent_syns.pop_front();
                    }
                    recent_syns.push_back(SynFingerprint {
                        src,
                        dst,
                        sport: tcp.source_port,
                        dport: tcp.destination_port,
                        seq: tcp.sequence_number,
                    });
                }
            }
        }
        // Truncate the `Bytes` to the actual amount the closure
        // consumed; smoltcp may not have written all `len` bytes.
        let _ = self.tx.send(used);
        result
    }
}

impl Device for ChannelPhy {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken;

    fn capabilities(&self) -> phy::DeviceCapabilities {
        let mut caps = phy::DeviceCapabilities::default();
        caps.max_transmission_unit = self.mtu;
        caps.medium = Medium::Ip;
        caps.checksum.ipv4 = phy::Checksum::Tx;
        caps.checksum.tcp = phy::Checksum::Tx;
        caps.checksum.udp = phy::Checksum::Tx;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.rx.pop_front().map(|buffer| {
            (
                RxToken { buffer },
                TxToken {
                    scratch: Arc::clone(&self.tx_scratch),
                    tx: self.tx.clone(),
                    recent_syns: Arc::clone(&self.recent_syns),
                },
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            scratch: Arc::clone(&self.tx_scratch),
            tx: self.tx.clone(),
            recent_syns: Arc::clone(&self.recent_syns),
        })
    }
}

#[derive(Clone, Copy)]
struct SynFingerprint {
    src: IpAddr,
    dst: IpAddr,
    sport: u16,
    dport: u16,
    seq: u32,
}

/// Retire request queued when a stream/socket owner is dropped.
#[derive(Clone, Copy)]
struct ReapRequest {
    handle: SocketHandle,
    kind: ReapKind,
    /// Ticks spent waiting for a TCP handshake to finish; force
    /// removal after `MAX_REAP_ATTEMPTS` so a wedged connection
    /// cannot pin its buffer forever.
    attempts: u32,
}

#[derive(Clone, Copy)]
enum ReapKind {
    /// Removed on the next drain tick (owner gone; queued response
    /// packets are useless).
    Udp,
    /// Removed once the TCP state machine reaches `Closed` (so the
    /// FIN exchange still goes out).
    Tcp,
}

/// 10ms stack poll tick × 600 ≈ 6s grace before force removal.
const MAX_REAP_ATTEMPTS: u32 = 600;

struct StackInner {
    iface: Interface,
    device: ChannelPhy,
    sockets: SocketSet<'static>,
    /// Reap requests not yet removable, retried on each drain tick.
    /// Invariant: a request for `handle` exists only while the
    /// original socket still occupies that slot — requests are
    /// enqueued by `Drop` (socket still in the set) and dequeued in
    /// the same drain that removes the socket, so an index reused by
    /// a later `SocketSet::add` can never be reclaimed by a stale
    /// request.
    pending_reaps: Vec<ReapRequest>,
}

impl StackInner {
    fn poll(&mut self, clock: &StackClock) {
        for _ in 0..4 {
            self.iface
                .poll(clock.now(), &mut self.device, &mut self.sockets);
        }
    }

    /// True when `handle` still refers to a live socket.
    /// `SocketSet::get[_mut]` panics on stale handles, so every path
    /// that may have had its socket reaped checks this first.
    fn has_socket(&self, handle: SocketHandle) -> bool {
        self.sockets.iter().any(|(h, _)| h == handle)
    }

    /// TCP state if the socket is still live, else `None`.
    fn tcp_state(&self, handle: SocketHandle) -> Option<tcp::State> {
        if self.has_socket(handle) {
            Some(self.sockets.get::<tcp::Socket>(handle).state())
        } else {
            None
        }
    }

    /// Process socket-reaping requests from dropped owners. UDP
    /// sockets are removed immediately; TCP sockets first get
    /// `close()` (idempotent — kicks off the FIN handshake even if
    /// the owner never called `poll_shutdown`, advances one step per
    /// tick) and are removed once `Closed`, or force-removed after
    /// `MAX_REAP_ATTEMPTS` (~6s) so nothing pins 128KB forever. The
    /// socket set previously only ever grew: every proxied TCP
    /// connection and every DNS lookup leaked for the process
    /// lifetime.
    fn drain_reap_queue(&mut self, incoming: &Mutex<Vec<ReapRequest>>) {
        {
            let mut q = incoming.lock();
            if !q.is_empty() {
                self.pending_reaps.append(&mut q);
            }
        }
        if self.pending_reaps.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.pending_reaps.len() {
            let req = self.pending_reaps[i];
            if !self.has_socket(req.handle) {
                self.pending_reaps.swap_remove(i);
                continue;
            }
            match req.kind {
                ReapKind::Udp => {
                    self.sockets.remove(req.handle);
                    self.pending_reaps.swap_remove(i);
                }
                ReapKind::Tcp => match self.tcp_state(req.handle) {
                    None => {
                        self.pending_reaps.swap_remove(i);
                    }
                    Some(tcp::State::Closed) => {
                        self.sockets.remove(req.handle);
                        self.pending_reaps.swap_remove(i);
                    }
                    Some(_) => {
                        if req.attempts + 1 >= MAX_REAP_ATTEMPTS {
                            self.sockets.remove(req.handle);
                            self.pending_reaps.swap_remove(i);
                        } else {
                            // Progress the handshake (no-op if
                            // already closing) then wait a tick.
                            self.sockets.get_mut::<tcp::Socket>(req.handle).close();
                            self.pending_reaps[i].attempts += 1;
                            i += 1;
                        }
                    }
                },
            }
        }
    }
}

pub struct StackShared {
    inner: Mutex<StackInner>,
    /// `Arc` (rather than a plain `Notify` in this struct) so parked
    /// readers/writers can register `OwnedNotified` waiters, which
    /// take `Arc<Notify>` by value.
    notify: Arc<Notify>,
    /// Sockets to retire once their owner (`VirtualTcpStream` /
    /// `VirtualUdpSocket`) is dropped. A separate sync mutex (like
    /// `inner`, its guard never crosses an `.await`, which the
    /// `!Send` guard type enforces at compile time); drained by the
    /// stack poll task under `inner`.
    reap: Mutex<Vec<ReapRequest>>,
    clock: StackClock,
}

pub struct VirtualTcpStream {
    handle: SocketHandle,
    shared: Arc<StackShared>,
    /// Read-side waiter registered on `StackShared::notify` while a
    /// read finds no data on an open socket. `OwnedNotified` is
    /// `!Unpin` (self-referential waiter node), hence `Pin<Box<_>>`;
    /// storing it across polls keeps one stable registration alive —
    /// returning `Pending` without a registered waker would park the
    /// task forever, and the old `wake_by_ref()` turned every idle
    /// connection into a busy loop (3 idle SOCKS connections pinned
    /// 2.6 CPU cores). Separate slot from `write_waiter` so a
    /// bidirectional relay can park on both directions at once.
    read_waiter: Option<Pin<Box<OwnedNotified>>>,
    /// Write-side waiter, same purpose for a full send window
    /// (`send_slice` returns `Ok(0)`): without it a blocked writer
    /// also busy-spun.
    write_waiter: Option<Pin<Box<OwnedNotified>>>,
}

impl AsyncRead for VirtualTcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut inner = match this.shared.inner.try_lock() {
            Some(inner) => inner,
            None => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        // Loop so that a fired waiter re-checks for data without
        // leaving the lock: registering the waiter must happen while
        // we still hold `inner`, otherwise the stack poll task can
        // process a packet and run `notify_waiters()` between our
        // empty read and the registration (lost wakeup, recovered
        // only by the 10ms fallback tick).
        loop {
            let socket = inner.sockets.get_mut::<tcp::Socket>(this.handle);
            let unfilled = buf.initialize_unfilled();
            match socket.recv_slice(unfilled) {
                Ok(0) => match socket.state() {
                    tcp::State::Closed | tcp::State::TimeWait => return Poll::Ready(Ok(())),
                    _ => {
                        inner.poll(&this.shared.clock);
                        // Idle open socket with no data: park on the
                        // shared notify instead of the old
                        // `wake_by_ref()` spin (3 idle held SOCKS
                        // connections pinned 2.6 CPU cores). Every
                        // stack poll — packet arrival *and* the 10ms
                        // tick — calls `notify_waiters()`, so FIN/RST
                        // transitions also wake us.
                        let waiter = this.read_waiter.get_or_insert_with(|| {
                            Box::pin(Arc::clone(&this.shared.notify).notified_owned())
                        });
                        match waiter.as_mut().poll(cx) {
                            Poll::Ready(()) => {
                                // Woken: drop the consumed waiter and
                                // re-check for data under the lock.
                                this.read_waiter = None;
                                continue;
                            }
                            Poll::Pending => return Poll::Pending,
                        }
                    }
                },
                Ok(n) => {
                    buf.advance(n);
                    // Data found without the waiter firing (e.g. a
                    // spurious task wake): discard any stale waiter.
                    this.read_waiter = None;
                    return Poll::Ready(Ok(()));
                }
                Err(tcp::RecvError::InvalidState) => {
                    return Poll::Ready(Err(io::ErrorKind::NotConnected.into()))
                }
                Err(tcp::RecvError::Finished) => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl AsyncWrite for VirtualTcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // A zero-length write must not park: `send_slice` reports
        // `Ok(0)` for an empty input, indistinguishable from "send
        // window full".
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let this = self.get_mut();
        let mut inner = match this.shared.inner.try_lock() {
            Some(inner) => inner,
            None => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        // Same lock-held registration pattern as `poll_read`: a
        // blocked writer (full send window) parks on the shared
        // notify, which the stack poll task signals after every
        // window-advancing ACK and on the 10ms tick, instead of the
        // old immediate `wake_by_ref()` busy-spin.
        loop {
            let socket = inner.sockets.get_mut::<tcp::Socket>(this.handle);
            match socket.send_slice(buf) {
                Ok(0) => {
                    inner.poll(&this.shared.clock);
                    let waiter = this.write_waiter.get_or_insert_with(|| {
                        Box::pin(Arc::clone(&this.shared.notify).notified_owned())
                    });
                    match waiter.as_mut().poll(cx) {
                        Poll::Ready(()) => {
                            this.write_waiter = None;
                            continue;
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }
                Ok(n) => {
                    inner.poll(&this.shared.clock);
                    this.write_waiter = None;
                    this.shared.notify.notify_waiters();
                    return Poll::Ready(Ok(n));
                }
                Err(tcp::SendError::InvalidState) => {
                    return Poll::Ready(Err(io::ErrorKind::NotConnected.into()))
                }
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(mut inner) = this.shared.inner.try_lock() {
            inner.sockets.get_mut::<tcp::Socket>(this.handle).close();
            inner.poll(&this.shared.clock);
            // Wake anyone parked in `poll_read`/`poll_write` so a
            // closed stream resolves instead of waiting for the
            // 10ms tick.
            this.shared.notify.notify_waiters();
        }
        Poll::Ready(Ok(()))
    }
}

pub struct VirtualUdpSocket {
    handle: SocketHandle,
    shared: Arc<StackShared>,
}

impl Drop for VirtualTcpStream {
    fn drop(&mut self) {
        // Free the smoltcp socket (128KB of TX/RX buffer) instead of
        // leaving it in the `SocketSet` forever. The poll task
        // finishes the FIN handshake before actually removing it
        // (or force-removes after ~6s).
        self.shared.reap.lock().push(ReapRequest {
            handle: self.handle,
            kind: ReapKind::Tcp,
            attempts: 0,
        });
        // Wake the poll task so the drain happens promptly even
        // without waiting for the 10ms tick to elapse naturally.
        self.shared.notify.notify_waiters();
    }
}

impl Drop for VirtualUdpSocket {
    fn drop(&mut self) {
        self.shared.reap.lock().push(ReapRequest {
            handle: self.handle,
            kind: ReapKind::Udp,
            attempts: 0,
        });
        self.shared.notify.notify_waiters();
    }
}

impl VirtualUdpSocket {
    pub async fn send_to(&self, data: &[u8], dest: SocketAddr) -> io::Result<()> {
        let meta = udp::UdpMetadata::from(IpEndpoint {
            addr: IpAddress::from(dest.ip()),
            port: dest.port(),
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut inner = self.shared.inner.lock();
                let socket = inner.sockets.get_mut::<udp::Socket>(self.handle);
                match socket.send_slice(data, meta) {
                    Ok(()) => {
                        inner.poll(&self.shared.clock);
                        self.shared.notify.notify_waiters();
                        return Ok(());
                    }
                    Err(udp::SendError::BufferFull) => {}
                    Err(udp::SendError::Unaddressable) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "udp send failed",
                        ));
                    }
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "udp send timed out",
                ));
            }
            let _ = tokio::time::timeout(Duration::from_millis(50), self.shared.notify.notified())
                .await;
        }
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut inner = self.shared.inner.lock();
                let socket = inner.sockets.get_mut::<udp::Socket>(self.handle);
                match socket.recv_slice(buf) {
                    Ok((n, meta)) => {
                        let addr = SocketAddr::new(
                            ip_address_to_std(meta.endpoint.addr),
                            meta.endpoint.port,
                        );
                        return Ok((n, addr));
                    }
                    Err(udp::RecvError::Exhausted) => {
                        inner.poll(&self.shared.clock);
                    }
                    Err(udp::RecvError::Truncated) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "udp truncated"));
                    }
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "udp recv timed out",
                ));
            }
            let _ = tokio::time::timeout(Duration::from_millis(50), self.shared.notify.notified())
                .await;
        }
    }
}

fn ip_address_to_std(addr: IpAddress) -> IpAddr {
    match addr {
        IpAddress::Ipv4(v4) => IpAddr::V4(v4),
        IpAddress::Ipv6(v6) => IpAddr::V6(v6),
    }
}

pub struct VirtualStack {
    shared: Arc<StackShared>,
    activity: Arc<Notify>,
    _poll_task: JoinHandle<()>,
}

impl VirtualStack {
    pub fn start(
        local_v4: Option<IpAddr>,
        local_v6: Option<IpAddr>,
        mtu: usize,
        from_tunnel: PacketRx,
        to_tunnel: PacketTx,
        activity: Arc<Notify>,
    ) -> Self {
        let mut device = ChannelPhy::new(to_tunnel.clone(), mtu);
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = rand::random();
        let clock = StackClock::new();
        let mut iface = Interface::new(config, &mut device, clock.now());

        iface.update_ip_addrs(|ip_addrs| {
            if let Some(v4) = local_v4 {
                let _ = ip_addrs.push(IpCidr::new(IpAddress::from(v4), 32));
            }
            if let Some(v6) = local_v6 {
                let _ = ip_addrs.push(IpCidr::new(IpAddress::from(v6), 128));
            }
        });

        if local_v4.is_some() {
            let _ = iface
                .routes_mut()
                .add_default_ipv4_route(Ipv4Address::UNSPECIFIED);
        }
        if local_v6.is_some() {
            let _ = iface
                .routes_mut()
                .add_default_ipv6_route(Ipv6Address::UNSPECIFIED);
        }

        let inner = StackInner {
            iface,
            device,
            sockets: SocketSet::new(vec![]),
            pending_reaps: Vec::new(),
        };

        let shared = Arc::new(StackShared {
            inner: Mutex::new(inner),
            notify: Arc::new(Notify::new()),
            reap: Mutex::new(Vec::new()),
            clock,
        });

        let poll_shared = Arc::clone(&shared);
        let poll_task = tokio::spawn(async move {
            run_poll_loop(from_tunnel, poll_shared).await;
        });

        Self {
            shared,
            activity,
            _poll_task: poll_task,
        }
    }

    pub fn wake(&self) {
        self.activity.notify_one();
    }

    pub async fn bind_udp(&self) -> io::Result<VirtualUdpSocket> {
        self.wake();

        let handle = {
            let mut inner = self.shared.inner.lock();
            let rx_buffer =
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 65535]);
            let tx_buffer =
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 65535]);
            let mut socket = udp::Socket::new(rx_buffer, tx_buffer);
            let local_port = 49152 + rand::random::<u16>() % 16384;
            socket
                .bind(IpListenEndpoint {
                    addr: None,
                    port: local_port,
                })
                .map_err(|_| io::Error::new(io::ErrorKind::AddrInUse, "udp bind failed"))?;
            inner.sockets.add(socket)
        };

        Ok(VirtualUdpSocket {
            handle,
            shared: Arc::clone(&self.shared),
        })
    }

    pub async fn dial_tcp(&self, addr: SocketAddr) -> io::Result<VirtualTcpStream> {
        self.wake();

        let handle = {
            let mut inner = self.shared.inner.lock();
            let rx_buffer = tcp::SocketBuffer::new(vec![0; 65535]);
            let tx_buffer = tcp::SocketBuffer::new(vec![0; 65535]);
            let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);
            let remote_ip = IpAddress::from(addr.ip());
            let local_port = 49152 + rand::random::<u16>() % 16384;
            socket
                .connect(inner.iface.context(), (remote_ip, addr.port()), local_port)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "tcp connect failed"))?;
            inner.sockets.add(socket)
        };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            {
                let mut inner = self.shared.inner.lock();
                inner.poll(&self.shared.clock);
                let state = inner.sockets.get::<tcp::Socket>(handle).state();
                match state {
                    tcp::State::Established => {
                        return Ok(VirtualTcpStream {
                            handle,
                            shared: Arc::clone(&self.shared),
                            read_waiter: None,
                            write_waiter: None,
                        });
                    }
                    tcp::State::Closed | tcp::State::TimeWait => {
                        // No stream owner will ever be created, so
                        // enqueue the reap here — otherwise a refused
                        // connection leaves its socket (and 128KB of
                        // buffer) in the set forever.
                        self.shared.reap.lock().push(ReapRequest {
                            handle,
                            kind: ReapKind::Tcp,
                            attempts: 0,
                        });
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            "tcp connection closed",
                        ));
                    }
                    _ => {}
                }
            }

            if tokio::time::Instant::now() >= deadline {
                self.shared.reap.lock().push(ReapRequest {
                    handle,
                    kind: ReapKind::Tcp,
                    attempts: 0,
                });
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "tcp connect timed out",
                ));
            }

            let _ = tokio::time::timeout(Duration::from_millis(50), self.shared.notify.notified())
                .await;
        }
    }

    pub async fn listen_tcp(&self, addr: SocketAddr) -> io::Result<TcpListener> {
        self.wake();
        TcpListener::bind(addr).await
    }
}

async fn run_poll_loop(from_tunnel: PacketRx, shared: Arc<StackShared>) {
    loop {
        tokio::select! {
            packet = from_tunnel.recv() => {
                let Ok(packet) = packet else { break };
                let mut inner = shared.inner.lock();
                inner.device.push_rx(packet);
                inner.poll(&shared.clock);
                inner.drain_reap_queue(&shared.reap);
                shared.notify.notify_waiters();
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                let mut inner = shared.inner.lock();
                inner.poll(&shared.clock);
                inner.drain_reap_queue(&shared.reap);
                shared.notify.notify_waiters();
            }
        }
    }
}
