mod enroll;
mod http_proxy;
mod nativetun;
mod portfw;
mod register;
mod socks;

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Register a new client and enroll a device key
    Register(register::RegisterArgs),
    /// Enroll a MASQUE private key
    Enroll(enroll::EnrollArgs),
    /// Expose Warp as a SOCKS5 proxy
    Socks(socks::SocksArgs),
    /// Expose Warp as an HTTP proxy with CONNECT support
    HttpProxy(http_proxy::HttpProxyArgs),
    /// Forward ports through a MASQUE tunnel
    PortFw(portfw::PortFwArgs),
    /// Expose Warp as a native TUN device
    NativeTun(nativetun::NativeTunArgs),
}

pub async fn execute(command: Commands, config_path: &str) -> Result<()> {
    match command {
        Commands::Register(args) => register::run(args, config_path).await,
        Commands::Enroll(args) => enroll::run(args, config_path).await,
        Commands::Socks(args) => socks::run(args, config_path).await,
        Commands::HttpProxy(args) => http_proxy::run(args, config_path).await,
        Commands::PortFw(args) => portfw::run(args, config_path).await,
        Commands::NativeTun(args) => nativetun::run(args, config_path).await,
    }
}
