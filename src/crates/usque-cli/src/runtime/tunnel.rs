use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use usque_config::Config;
use usque_crypto::{
    build_rustls_config, decode_endpoint_public_key, decode_private_key,
    prepare_quiche_client_credentials, TlsOptions,
};
use usque_masque::ConnectOptions;

use super::TunnelFlags;

pub fn build_connect_options(config: &Config, flags: &TunnelFlags) -> Result<ConnectOptions> {
    let signing_key = decode_private_key(&config.private_key)?;
    let peer_key = decode_endpoint_public_key(&config.endpoint_pub_key)?;

    let alpn = if flags.http2 {
        vec![b"h2".to_vec()]
    } else {
        vec![b"h3".to_vec()]
    };

    let tls_config = build_rustls_config(
        &signing_key,
        TlsOptions {
            sni: flags.sni_address.clone(),
            insecure: flags.insecure,
            peer_public_key: peer_key.clone(),
            alpn,
        },
    )?;

    let quiche_credentials = Arc::new(prepare_quiche_client_credentials(&signing_key)?);

    let endpoint =
        usque_config::select_endpoint(config, flags.http2, flags.ipv6, flags.connect_port)?;

    if flags.insecure {
        tracing::warn!("WARNING: --insecure is set, endpoint certificate pinning is disabled");
    }
    if flags.http2 {
        tracing::info!("HTTP/2 mode enabled. See {}", usque_config::HTTP2_WIKI_URL);
        tracing::info!("Using HTTP/2 endpoint {endpoint}");
    }

    Ok(ConnectOptions {
        tls_config,
        quiche_credentials,
        insecure: flags.insecure,
        peer_public_key: peer_key,
        endpoint,
        sni: flags.sni_address.clone(),
        use_http2: flags.http2,
        keepalive_period: Duration::from(flags.keepalive_period),
        initial_packet_size: flags.initial_packet_size,
        connect_uri: usque_cloudflare_api::CONNECT_URI.to_string(),
    })
}

pub fn build_native_connect(config: &Config, flags: &TunnelFlags) -> Result<ConnectOptions> {
    build_connect_options(config, flags)
}
