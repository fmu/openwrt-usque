mod client;
mod models;

pub use client::{enroll_key, register, ApiError, CloudflareClient};
pub use models::*;

pub const API_URL: &str = "https://api.cloudflareclient.com";
pub const API_VERSION: &str = "v0a4471";
pub const CONNECT_SNI: &str = "consumer-masque.cloudflareclient.com";
pub const CONNECT_URI: &str = "https://cloudflareaccess.com";
pub const DEFAULT_MODEL: &str = "PC";
pub const DEFAULT_LOCALE: &str = "en_US";
pub const KEY_TYPE_WG: &str = "curve25519";
pub const TUN_TYPE_WG: &str = "wireguard";
pub const KEY_TYPE_MASQUE: &str = "secp256r1";
pub const TUN_TYPE_MASQUE: &str = "masque";
