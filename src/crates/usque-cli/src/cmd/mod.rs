mod enroll;
mod nativetun;
mod register;

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Register a new client and enroll a device key
    Register(register::RegisterArgs),
    /// Enroll a MASQUE private key
    Enroll(enroll::EnrollArgs),
    /// Expose Warp as a native TUN device
    NativeTun(nativetun::NativeTunArgs),
}

pub async fn execute(command: Commands, config_path: &str) -> Result<()> {
    match command {
        Commands::Register(args) => register::run(args, config_path).await,
        Commands::Enroll(args) => enroll::run(args, config_path).await,
        Commands::NativeTun(args) => nativetun::run(args, config_path).await,
    }
}
