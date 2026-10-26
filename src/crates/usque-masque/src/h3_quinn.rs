use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use bytes::{BufMut, Bytes};
use crossfire::mpsc;
use futures_util::future;
use http::Uri;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tracing::{debug, warn};

use h3_datagram::datagram_handler::HandleDatagramsExt;
use usque_crypto::init as init_crypto;

use crate::capsule::CapsuleReader;
use crate::connect_ip::{ConnectIpSession, Transport};

pub async fn connect_h3(options: &crate::session::ConnectOptions) -> Result<ConnectIpSession> {
    init_crypto();

    let bind_ip = match options.endpoint.ip() {
        std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    };

    let mut endpoint =
        quinn::Endpoint::client((bind_ip, 0).into()).context("failed to create QUIC endpoint")?;

    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(Arc::clone(&options.tls_config))
            .map_err(|err| anyhow!("invalid TLS client config for QUIC: {err}"))?,
    ));
    let mut transport = quinn::TransportConfig::default();
    if options.initial_packet_size > 0 {
        transport.initial_mtu(options.initial_packet_size as u16);
    }
    // Keep the QUIC connection alive across idle periods instead of timing out.
    let keepalive = if options.keepalive_period.is_zero() {
        std::time::Duration::from_secs(30)
    } else {
        options.keepalive_period
    };
    transport.keep_alive_interval(Some(keepalive));
    // Do not terminate the tunnel on inactivity. Idle tunnels are simply
    // dormant; keep-alives preserve the session for subsequent activity.
    transport.max_idle_timeout(None);
    client_config.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(client_config);

    let conn = endpoint
        .connect(options.endpoint, &options.sni)
        .context("failed to dial QUIC endpoint")?
        .await
        .context("QUIC dial failed")?;

    // Keep a handle for direct DATAGRAM sends: routing them through
    // h3-quinn's `DatagramSender` would `copy_to_bytes` the header and
    // payload into a fresh buffer (a full extra memcpy per packet).
    let conn_send = conn.clone();
    let h3_conn = h3_quinn::Connection::new(conn);
    let (mut driver, mut send_request) = h3::client::builder()
        .enable_datagram(true)
        .enable_extended_connect(true)
        // `B` (the frame body buffer type) is no longer constrained by a
        // `DatagramSender` — datagrams now go straight to quinn — so pin
        // it explicitly to `Bytes`.
        .build::<_, _, Bytes>(h3_conn)
        .await
        .map_err(|err| anyhow!("failed to build HTTP/3 client: {err}"))?;

    let (mut request_stream, response) = {
        let uri: Uri = options.connect_uri.parse().context("invalid connect URI")?;
        let protocol = h3::ext::Protocol::CF_CONNECT_IP;
        let request = http::Request::builder()
            .method(http::Method::CONNECT)
            .uri(uri)
            .header("capsule-protocol", "?1")
            .header("user-agent", "")
            .extension(protocol)
            .body(())
            .context("failed to build CONNECT request")?;

        let mut request_stream = send_request
            .send_request(request)
            .await
            .map_err(|err| anyhow!("failed to send CONNECT request: {err}"))?;
        let response = request_stream
            .recv_response()
            .await
            .map_err(|err| anyhow!("CONNECT response failed: {err}"))?;
        (request_stream, response)
    };

    let status = response.status().as_u16();
    if status != 200 {
        if status == 403 {
            anyhow::bail!(
                "login failed! Please double-check if your tls key and cert is enrolled in the Cloudflare Access service"
            );
        }
        anyhow::bail!("tunnel connection failed: {status}");
    }
    debug!("HTTP/3 CONNECT-IP established: {status}");

    let stream_id = request_stream.id();
    let mut datagram_reader = driver.get_datagram_reader();
    // Pre-encode the quarter-stream-id varint once per session; the
    // sender task prepends it to every payload without re-encoding.
    let mut header_buf = bytes::BytesMut::new();
    crate::capsule::put_varint(&mut header_buf, stream_id.into_inner() / 4);
    let header = header_buf.freeze();
    let header_out = header.clone();

    let (out_tx, out_rx) = mpsc::bounded_async::<Vec<u8>>(64);
    let incoming = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let notify = Arc::new(Notify::new());
    let closed = Arc::new(AtomicBool::new(false));

    // Drive the HTTP/3 connection state machine on a background task.
    tokio::spawn(async move {
        let _ = future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    // Drain the CONNECT response body (control-channel capsules this
    // client doesn't act on) and treat the stream as the connection's
    // lifetime gauge. Keep `send_request` alive here: dropping the last
    // `SendRequest` sends H3_NO_ERROR "Connection closed by client" and
    // tears down the connection.
    let closed_recv = Arc::clone(&closed);
    let notify_recv = Arc::clone(&notify);
    tokio::spawn(async move {
        let _send_request = send_request;
        let mut reader = CapsuleReader::new();
        loop {
            match request_stream.recv_data().await {
                Ok(Some(mut data)) => {
                    use bytes::Buf;
                    let chunk: Bytes = data.copy_to_bytes(data.remaining());
                    reader.push(chunk);
                    while reader.next_ip_packet().is_some() {}
                }
                Ok(None) | Err(_) => break,
            }
        }
        closed_recv.store(true, Ordering::Relaxed);
        notify_recv.notify_waiters();
    });

    // Incoming IP packets arrive as H3 DATAGRAMs on the CONNECT stream.
    let incoming_recv = Arc::clone(&incoming);
    let closed_dgram = Arc::clone(&closed);
    let notify_dgram = Arc::clone(&notify);
    tokio::spawn(async move {
        loop {
            match datagram_reader.read_datagram().await {
                Ok(dgram) => {
                    // `dgram` is an owned `Datagram<Bytes>` already advanced
                    // past the quarter-stream-id varint by `Datagram::decode`;
                    // `into_payload` moves the `Bytes` out without copying.
                    let bytes = dgram.into_payload();
                    if let Some(packet) = crate::datagram::decode_h3_datagram_payload_owned(&bytes)
                    {
                        incoming_recv.lock().push_back(packet);
                        notify_dgram.notify_waiters();
                    }
                }
                Err(err) => {
                    warn!("h3 datagram read error: {err}");
                    closed_dgram.store(true, Ordering::Relaxed);
                    notify_dgram.notify_waiters();
                    break;
                }
            }
        }
    });

    // Outgoing packets are sent as H3 DATAGRAMs bound to the CONNECT stream.
    // Periodically emit a tiny heartbeat datagram so the QUIC connection and
    // CONNECT-IP session stay alive across application-level idle gaps
    // (transport keep-alive PINGs alone were insufficient).
    let keepalive_tx = keepalive;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(keepalive_tx);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `write_packet` already emits the final wire layout
        // (quarter-stream-id header + context-id + packet) as one
        // contiguous Vec, so wrapping it into `Bytes` is a pure move —
        // no concat, no copy.
        loop {
            tokio::select! {
                msg = out_rx.recv() => {
                    match msg {
                        Ok(payload) => {
                            // `send_datagram_wait` blocks (applies backpressure)
                            // when the datagram send buffer is full instead of
                            // dropping oldest datagrams like `send_datagram` does.
                            match conn_send
                                .send_datagram_wait(Bytes::from(payload))
                                .await
                            {
                                Ok(()) => {}
                                // Unrecoverable for this packet only (e.g. an
                                // oversized IP packet): drop it and keep the
                                // tunnel running. Only breaking here would
                                // silently wedge the outbound channel.
                                Err(quinn::SendDatagramError::TooLarge) => {
                                    warn!("h3 datagram too large, dropping packet");
                                }
                                Err(err) => {
                                    warn!("h3 datagram send error: {err}");
                                    break;
                                }
                            }
                        }
                        // All senders dropped (session torn down): exit the task.
                        Err(_) => break,
                    }
                }
                _ = tick.tick() => {
                    let mut ping = bytes::BytesMut::with_capacity(header.len() + 1);
                    ping.put_slice(&header);
                    crate::capsule::put_varint(&mut ping, 0);
                    if conn_send.send_datagram_wait(ping.freeze()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(ConnectIpSession::with_capacity(
        incoming,
        notify,
        Transport::H3Quinn {
            header: header_out,
            out: out_tx,
        },
        closed,
        1500,
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use h3_datagram::datagram::Datagram;

    /// The inbound datagram path must not copy: `Datagram::decode`
    /// advances past the quarter-stream-id varint, `into_payload`
    /// moves the `Bytes` out, and `decode_h3_datagram_payload_owned`
    /// slices off the context-id varint. All three steps only bump
    /// the refcount, so the decoded packet aliases the wire buffer.
    #[test]
    fn inbound_payload_aliases_wire_buffer() {
        // Wire layout: quarter-stream-id varint (1 byte, value 0) +
        // context-id varint (1 byte, value 0) + IPv4 TCP packet.
        let mut packet = vec![0u8; 20];
        packet[0] = 0x45;
        packet[2] = 0x00;
        packet[3] = 0x14;
        packet[8] = 63;
        packet[9] = 6;

        let mut wire = vec![0u8, 0u8];
        wire.extend_from_slice(&packet);
        let wire = Bytes::from(wire);

        let dgram = Datagram::decode(wire.clone()).expect("valid datagram");
        let payload = dgram.into_payload();
        let decoded =
            crate::datagram::decode_h3_datagram_payload_owned(&payload).expect("valid payload");

        assert_eq!(decoded.len(), packet.len());
        assert_eq!(&decoded[..], &packet[..]);
        // 1 byte quarter-stream-id + 1 byte context-id prefix skipped,
        // no copy in between.
        assert_eq!(
            decoded.as_ptr(),
            unsafe { wire.as_ptr().add(2) },
            "decoded packet must alias the wire buffer"
        );
    }
}
