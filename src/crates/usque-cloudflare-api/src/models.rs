use serde::{Deserialize, Serialize};

pub const INVALID_PUBLIC_KEY: &str = "Invalid public key";

#[derive(Debug, Clone, Serialize)]
pub struct Registration {
    pub key: String,
    pub install_id: String,
    pub fcm_token: String,
    pub tos: String,
    pub model: String,
    #[serde(rename = "serial_number")]
    pub serial: String,
    pub os_version: String,
    pub key_type: String,
    #[serde(rename = "tunnel_type")]
    pub tun_type: String,
    pub locale: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountData {
    pub id: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub key: String,
    #[serde(rename = "key_type", default)]
    pub key_type: String,
    #[serde(rename = "tunnel_type", default)]
    pub tun_type: String,
    pub account: Account,
    pub config: WarpConfig,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Account {
    #[serde(default)]
    pub license: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WarpConfig {
    pub peers: Vec<Peer>,
    pub interface: InterfaceConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InterfaceConfig {
    pub addresses: AddressPair,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AddressPair {
    pub v4: String,
    pub v6: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Peer {
    pub public_key: String,
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Endpoint {
    pub v4: String,
    pub v6: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceUpdate {
    pub key: String,
    pub key_type: String,
    #[serde(rename = "tunnel_type")]
    pub tun_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorBody {
    pub success: bool,
    pub errors: Vec<ErrorInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorInfo {
    pub code: i32,
    pub message: String,
}

impl ApiErrorBody {
    pub fn has_error_message(&self, message: &str) -> bool {
        self.errors.iter().any(|e| e.message == message)
    }

    pub fn errors_as_string(&self, separator: &str) -> String {
        self.errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join(separator)
    }
}
