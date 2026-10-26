//! Integration harness: open a MASQUE tunnel and shuttle packets between a
//! channel device and Cloudflare. Requires a valid `config.json`.
//!
//! ```bash
//! cargo run -p usque-tunnel-core --example tunnel_harness -- --config config.json
//! ```

use std::sync::Arc;
use std::time::Duration;

use usque_config::Config;
use usque_crypto::{
    build_rustls_config, decode_endpoint_public_key, decode_private_key, init,
    prepare_quiche_client_credentials, TlsOptions,
};
use usque_masque::ConnectOptions;
use usque_tunnel_core::{MaintainTunnelConfig, TunnelSupervisor};
use usque_virtual_net::ChannelDevice;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("usque=info".parse()?),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .filter(|a| a != "--config")
        .or_else(|| {
            std::env::args()
                .position(|a| a == "--config")
                .and_then(|i| std::env::args().nth(i + 1))
        })
        .unwrap_or_else(|| "config.json".to_string());

    let cfg = Config::load(&config_path)?;
    init();
    let signing_key = decode_private_key(&cfg.private_key)?;
    let peer_key = decode_endpoint_public_key(&cfg.endpoint_pub_key)?;
    let tls_config = build_rustls_config(
        &signing_key,
        TlsOptions {
            sni: usque_cloudflare_api::CONNECT_SNI.to_string(),
            insecure: false,
            peer_public_key: peer_key.clone(),
            alpn: vec![b"h3".to_vec()],
        },
    )?;

    let endpoint = usque_config::select_endpoint(&cfg, false, false, 443)?;
    let connect = ConnectOptions {
        tls_config,
        quiche_credentials: Arc::new(prepare_quiche_client_credentials(&signing_key)?),
        insecure: false,
        peer_public_key: peer_key,
        endpoint,
        sni: usque_cloudflare_api::CONNECT_SNI.to_string(),
        use_http2: false,
        keepalive_period: Duration::from_secs(30),
        initial_packet_size: 0,
        connect_uri: usque_cloudflare_api::CONNECT_URI.to_string(),
    };

    let (device, to_device, _from_device) = ChannelDevice::pair();
    let device = Arc::new(device);

    // Wake the tunnel by sending a minimal outbound packet (ICMP echo stub).
    let wake = to_device.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = wake.send(bytes::Bytes::from(vec![0u8; 64]));
    });

    let maintain = MaintainTunnelConfig {
        connect,
        mtu: 1280,
        reconnect_delay: Duration::from_secs(1),
        always_reconnect: false,
        on_connect: String::new(),
        on_disconnect: String::new(),
        hook_env: usque_tunnel_core::HookEnv::default().with("USQUE_MODE", "harness"),
        activity: None,
    };

    tracing::info!("Starting tunnel harness (config: {config_path})");
    TunnelSupervisor::maintain(maintain, device).await;
    Ok(())
}
