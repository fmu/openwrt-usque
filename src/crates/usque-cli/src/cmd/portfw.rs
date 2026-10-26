use std::sync::Arc;

use anyhow::Result;
use clap::Args;

use usque_portfw::{kick_tunnel, parse_port_mapping, run_local_forwards, run_remote_forwards};

use crate::runtime::{load_config, spawn_userspace_tunnel, TunnelFlags};

#[derive(Debug, Args)]
pub struct PortFwArgs {
    #[command(flatten)]
    pub tunnel: TunnelFlags,
    #[arg(short = 'L', long = "local-ports")]
    pub local_ports: Vec<String>,
    #[arg(short = 'R', long = "remote-ports")]
    pub remote_ports: Vec<String>,
    #[arg(long = "dont-always-reconnect")]
    pub dont_always_reconnect: bool,
}

pub async fn run(mut args: PortFwArgs, config_path: &str) -> Result<()> {
    args.tunnel.always_reconnect = !args.dont_always_reconnect;

    let config = load_config(config_path)?;
    let (stack, _device, _handle) = spawn_userspace_tunnel(&config, &args.tunnel, "portfw")?;

    let local = args
        .local_ports
        .iter()
        .map(|s| parse_port_mapping(s))
        .collect::<Result<Vec<_>>>()?;
    let remote = args
        .remote_ports
        .iter()
        .map(|s| parse_port_mapping(s))
        .collect::<Result<Vec<_>>>()?;

    run_local_forwards(local, Arc::clone(&stack)).await;
    run_remote_forwards(remote, stack.clone()).await;

    kick_tunnel(&stack);
    tracing::info!("Successfully connected to Cloudflare");

    std::future::pending::<()>().await;
    Ok(())
}
