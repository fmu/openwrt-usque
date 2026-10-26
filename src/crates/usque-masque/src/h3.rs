use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use boring::ssl::{
    SslAlert, SslContextBuilder, SslFiletype, SslMethod, SslVerifyError, SslVerifyMode,
};
use bytes::Bytes;
use crossfire::mpsc;
use futures_util::SinkExt;
use http::Uri;
use parking_lot::Mutex;
use tokio_quiche::datagram_socket::DgramBuffer;
use tokio_quiche::http3::driver::{
    ClientH3Event, H3Event, InboundFrame, IncomingH3Headers, NewClientRequest, OutboundFrame,
};

/// Headroom reserved in front of every outbound payload so the
/// tokio-quiche driver can prepend the quarter-stream-id varint in
/// place (`try_add_prefix`) without shifting or copying the payload.
/// Matches datagram-socket's `DGRAM_HEADROOM` contract (>= 8 bytes).
const QUARTER_SID_HEADROOM: usize = 8;
use tokio_quiche::http3::settings::Http3Settings;
use tokio_quiche::quic::ConnectionHook;
use tokio_quiche::quiche::h3::{self, NameValue};
use tokio_quiche::settings::{
    CertificateKind, ConnectionParams, Hooks, QuicSettings, TlsCertificatePaths,
};
use tokio_quiche::{ClientH3Controller, ClientH3Driver};
use tracing::debug;

use usque_crypto::{init as init_crypto, PeerPublicKey};

use crate::capsule::CapsuleReader;
use crate::connect_ip::{ConnectIpSession, H3ConnectionGuard, Transport};

struct PinnedKeyHook {
    expected: PeerPublicKey,
}

impl ConnectionHook for PinnedKeyHook {
    fn create_custom_ssl_context_builder(
        &self,
        settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let mut builder = SslContextBuilder::new(SslMethod::tls()).ok()?;
        builder.set_certificate_chain_file(settings.cert).ok()?;
        builder
            .set_private_key_file(settings.private_key, SslFiletype::PEM)
            .ok()?;

        let expected = self.expected.clone();
        builder.set_custom_verify_callback(SslVerifyMode::PEER, move |ssl| {
            let cert = ssl
                .peer_certificate()
                .ok_or(SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN))?;
            let pkey = cert
                .public_key()
                .map_err(|_| SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN))?;
            let der = pkey
                .public_key_to_der()
                .map_err(|_| SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN))?;
            if der.as_slice() != expected.as_der() {
                return Err(SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN));
            }
            Ok(())
        });

        Some(builder)
    }
}

pub async fn connect_h3(options: &crate::session::ConnectOptions) -> Result<ConnectIpSession> {
    init_crypto();

    let bind_ip = match options.endpoint.ip() {
        std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    };
    let socket = tokio::net::UdpSocket::bind((bind_ip, 0))
        .await
        .context("failed to bind UDP socket")?;
    socket
        .connect(options.endpoint)
        .await
        .context("failed to connect UDP socket")?;

    let mut http3_settings = Http3Settings::default();
    http3_settings.enable_extended_connect = true;
    let (h3_driver, mut controller) = ClientH3Driver::new(http3_settings);

    let mut quic_settings = QuicSettings::default();
    quic_settings.alpn = vec![b"h3".to_vec()];
    quic_settings.enable_dgram = true;
    quic_settings.verify_peer = !options.insecure;
    if options.initial_packet_size > 0 {
        let mtu = options.initial_packet_size as usize;
        quic_settings.max_send_udp_payload_size = mtu;
        quic_settings.max_recv_udp_payload_size = mtu;
        quic_settings.discover_path_mtu = false;
    }

    let cert_path = options
        .quiche_credentials
        .cert_path
        .to_str()
        .context("invalid client certificate path")?;
    let key_path = options
        .quiche_credentials
        .key_path
        .to_str()
        .context("invalid client private key path")?;
    let tls_cert = TlsCertificatePaths {
        cert: cert_path,
        private_key: key_path,
        kind: CertificateKind::X509,
    };

    let hooks = if options.insecure {
        Hooks::default()
    } else {
        Hooks {
            connection_hook: Some(Arc::new(PinnedKeyHook {
                expected: options.peer_public_key.clone(),
            })),
        }
    };

    let params = ConnectionParams::new_client(quic_settings, Some(tls_cert), hooks);
    let socket = socket
        .try_into()
        .map_err(|err| anyhow!("failed to convert UDP socket for tokio-quiche: {err:?}"))?;
    let conn = match tokio_quiche::quic::connect_with_config(
        socket,
        Some(&options.sni),
        &params,
        h3_driver,
    )
    .await
    {
        Ok(conn) => conn,
        Err(err) => return Err(anyhow!("QUIC dial failed: {err}")),
    };

    send_connect_request(&mut controller, options)?;

    let (dgram_out_tx, dgram_out_rx) = mpsc::bounded_async::<Vec<u8>>(64);
    let incoming = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let notify = Arc::new(tokio::sync::Notify::new());
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut flow_send = None;
    let mut flow_id = None;
    let mut established = false;

    while !established {
        let Some(event) = controller.event_receiver_mut().recv().await else {
            anyhow::bail!("HTTP/3 controller closed before CONNECT established");
        };

        match event {
            ClientH3Event::NewOutboundRequest { .. } => {}
            ClientH3Event::Core(H3Event::IncomingSettings { .. }) => {}
            ClientH3Event::Core(H3Event::NewFlow {
                flow_id: fid,
                send,
                mut recv,
            }) if flow_send.is_none() => {
                flow_id = Some(fid);
                flow_send = Some(send);
                let incoming_recv = Arc::clone(&incoming);
                let notify_recv = Arc::clone(&notify);
                let closed_recv = Arc::clone(&closed);
                tokio::spawn(async move {
                    loop {
                        if closed_recv.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(frame) = recv.recv().await else {
                            break;
                        };
                        if let InboundFrame::Datagram(dgram) = frame {
                            // dgram is an owned DgramBuffer (Vec<u8> wrapper).
                            // `into_parts` moves the inner Vec out (no copy) and
                            // reports the read cursor; `Bytes::from(vec)` wraps
                            // it and `.slice(start..)` only bumps the refcount,
                            // so the downstream pipeline stays zero-copy.
                            let (vec, start) = dgram.into_parts();
                            let bytes = Bytes::from(vec).slice(start..);
                            if let Some(packet) =
                                crate::datagram::decode_h3_datagram_payload_owned(&bytes)
                            {
                                incoming_recv.lock().push_back(packet);
                                notify_recv.notify_waiters();
                            }
                        }
                    }
                    // The flow's datagram stream ended: mark the
                    // session closed and wake the supervisor so
                    // `read_packet` returns `Ok(None)` (reconnect)
                    // instead of parking forever.
                    closed_recv.store(true, Ordering::Relaxed);
                    notify_recv.notify_waiters();
                });
            }
            ClientH3Event::Core(H3Event::IncomingHeaders(headers)) => {
                let status = response_status(&headers);
                if status != 200 {
                    if status == 403 {
                        anyhow::bail!(
                            "login failed! Please double-check if your tls key and cert is enrolled in the Cloudflare Access service"
                        );
                    }
                    anyhow::bail!("tunnel connection failed: {status}");
                }

                debug!("HTTP/3 CONNECT-IP established: {status}");
                spawn_control_stream_reader(headers);
                established = true;
            }
            ClientH3Event::Core(H3Event::ConnectionError(err)) => {
                anyhow::bail!("HTTP/3 connection error: {err:?}");
            }
            ClientH3Event::Core(H3Event::ConnectionShutdown(err)) => {
                anyhow::bail!("HTTP/3 connection shutdown: {err:?}");
            }
            _ => {}
        }
    }

    let send = flow_send.ok_or_else(|| anyhow!("CONNECT-IP datagram flow was not created"))?;
    let fid = flow_id.ok_or_else(|| anyhow!("CONNECT-IP datagram flow ID missing"))?;
    tokio::spawn(async move {
        let mut send = send;
        while let Ok(payload) = dgram_out_rx.recv().await {
            // The payload Vec already carries QUARTER_SID_HEADROOM zero
            // bytes up front (reserved by `write_packet`); wrapping it
            // moves the buffer instead of copying it, and the driver
            // prepends the quarter-stream-id into that headroom.
            let dgram = DgramBuffer::from_vec_with_headroom(payload, QUARTER_SID_HEADROOM);
            if send
                .send(OutboundFrame::Datagram(dgram, fid))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    Ok(ConnectIpSession::with_capacity(
        incoming,
        notify,
        Transport::H3Quiche {
            out: dgram_out_tx,
            _guard: Arc::new(H3ConnectionGuard {
                _conn: conn,
                _controller: controller,
            }),
        },
        closed,
        // Reserve an MTU + datagram prefix so per-packet `write_packet`
        // doesn't reallocate the scratch BytesMut.
        1500,
    ))
}

fn send_connect_request(
    controller: &mut ClientH3Controller,
    options: &crate::session::ConnectOptions,
) -> Result<()> {
    let uri: Uri = options.connect_uri.parse().context("invalid connect URI")?;
    let authority = uri.authority().map(|a| a.as_str()).unwrap_or("").as_bytes();
    let path = uri.path().as_bytes();

    let headers = vec![
        h3::Header::new(b":method", b"CONNECT"),
        h3::Header::new(b":scheme", b"https"),
        h3::Header::new(b":authority", authority),
        h3::Header::new(b":path", path),
        h3::Header::new(b":protocol", b"cf-connect-ip"),
        h3::Header::new(b"capsule-protocol", b"?1"),
        h3::Header::new(b"user-agent", b""),
    ];

    controller
        .request_sender()
        .send(NewClientRequest {
            request_id: 0,
            headers,
            body_writer: None,
        })
        .map_err(|_| anyhow!("failed to enqueue CONNECT request"))
}

fn response_status(headers: &IncomingH3Headers) -> u16 {
    headers
        .headers
        .iter()
        .find(|header| header.name() == b":status")
        .and_then(|header| std::str::from_utf8(header.value()).ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn spawn_control_stream_reader(headers: IncomingH3Headers) {
    let mut recv = headers.recv;
    tokio::spawn(async move {
        let mut reader = CapsuleReader::new();
        while let Some(frame) = recv.recv().await {
            match frame {
                InboundFrame::Body(data, fin) => {
                    let bytes = data.freeze();
                    reader.push(bytes);
                    // Drain any complete packets; the control stream
                    // carries address-assignment / route-advertisement
                    // capsules that this client doesn't act on, so we
                    // discard the parsed values.
                    while let Some(_pkt) = reader.next_ip_packet() {
                        // ignore
                    }
                    if fin {
                        break;
                    }
                }
                InboundFrame::Datagram(_) => {}
            }
        }
    });
}
