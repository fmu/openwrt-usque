use std::io::{self, Write};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::Client;

use crate::models::{AccountData, ApiErrorBody, DeviceUpdate, Registration};
use crate::{
    API_URL, API_VERSION, DEFAULT_LOCALE, DEFAULT_MODEL, KEY_TYPE_MASQUE, KEY_TYPE_WG,
    TUN_TYPE_MASQUE, TUN_TYPE_WG,
};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub body: ApiErrorBody,
    pub status: reqwest::StatusCode,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "API error {}: {}",
            self.status,
            self.body.errors_as_string("; ")
        )
    }
}

pub struct CloudflareClient {
    http: Client,
}

impl Default for CloudflareClient {
    fn default() -> Self {
        Self {
            http: Client::new(),
        }
    }
}

impl CloudflareClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn default_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("WARP for Android"));
        headers.insert("CF-Client-Version", HeaderValue::from_static("a-6.35-4471"));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=UTF-8"),
        );
        headers.insert("Connection", HeaderValue::from_static("Keep-Alive"));
        headers
    }

    pub fn cf_time_string() -> String {
        // Match Go TimeAsCfString: 2006-01-02T15:04:05.000-07:00
        chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
            .to_string()
    }
}

fn prompt_yes_no(message: &str) -> Result<bool> {
    print!("{message}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim() == "y")
}

pub fn generate_random_wg_pubkey() -> Result<String> {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    Ok(STANDARD.encode(key))
}

pub fn generate_random_android_serial() -> Result<String> {
    let mut serial = [0u8; 8];
    rand::rng().fill_bytes(&mut serial);
    Ok(hex::encode(serial))
}

pub async fn register(
    client: &CloudflareClient,
    model: &str,
    locale: &str,
    jwt: Option<&str>,
    accept_tos: bool,
) -> Result<AccountData> {
    let model = if model.is_empty() {
        DEFAULT_MODEL
    } else {
        model
    };
    let locale = if locale.is_empty() {
        DEFAULT_LOCALE
    } else {
        locale
    };

    if !accept_tos
        && !prompt_yes_no(
            "You must accept the Terms of Service (https://www.cloudflare.com/application/terms/) to register. Do you agree? (y/n): ",
        )?
    {
        bail!("user did not accept TOS");
    }

    let data = Registration {
        key: generate_random_wg_pubkey()?,
        install_id: String::new(),
        fcm_token: String::new(),
        tos: CloudflareClient::cf_time_string(),
        model: model.to_string(),
        serial: generate_random_android_serial()?,
        os_version: String::new(),
        key_type: KEY_TYPE_WG.to_string(),
        tun_type: TUN_TYPE_WG.to_string(),
        locale: locale.to_string(),
    };

    let url = format!("{API_URL}/{API_VERSION}/reg");
    let mut req = client
        .http
        .post(url)
        .headers(CloudflareClient::default_headers())
        .json(&data);

    if let Some(jwt) = jwt.filter(|s| !s.is_empty()) {
        req = req.header("CF-Access-Jwt-Assertion", jwt);
    }

    let resp = req
        .send()
        .await
        .context("failed to send registration request")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if body.is_empty() {
            bail!("failed to register: {status}");
        }
        bail!("failed to register: {status}: {body}");
    }

    resp.json::<AccountData>()
        .await
        .context("failed to decode registration response")
}

pub async fn enroll_key(
    client: &CloudflareClient,
    device_id: &str,
    access_token: &str,
    pub_key: &[u8],
    device_name: &str,
) -> Result<AccountData, ApiError> {
    let update = DeviceUpdate {
        key: STANDARD.encode(pub_key),
        key_type: KEY_TYPE_MASQUE.to_string(),
        tun_type: TUN_TYPE_MASQUE.to_string(),
        name: device_name.to_string(),
    };

    let url = format!("{API_URL}/{API_VERSION}/reg/{device_id}");
    let resp = client
        .http
        .patch(url)
        .headers(CloudflareClient::default_headers())
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .json(&update)
        .send()
        .await
        .map_err(|e| ApiError {
            body: ApiErrorBody {
                success: false,
                errors: vec![crate::models::ErrorInfo {
                    code: 0,
                    message: e.to_string(),
                }],
            },
            status: reqwest::StatusCode::BAD_REQUEST,
        })?;

    let status = resp.status();
    let body = resp.bytes().await.map_err(|e| ApiError {
        body: ApiErrorBody {
            success: false,
            errors: vec![crate::models::ErrorInfo {
                code: 0,
                message: e.to_string(),
            }],
        },
        status,
    })?;

    if !status.is_success() {
        let api_err: ApiErrorBody = serde_json::from_slice(&body).map_err(|e| ApiError {
            body: ApiErrorBody {
                success: false,
                errors: vec![crate::models::ErrorInfo {
                    code: 0,
                    message: e.to_string(),
                }],
            },
            status,
        })?;
        return Err(ApiError {
            body: api_err,
            status,
        });
    }

    serde_json::from_slice(&body).map_err(|e| ApiError {
        body: ApiErrorBody {
            success: false,
            errors: vec![crate::models::ErrorInfo {
                code: 0,
                message: e.to_string(),
            }],
        },
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::CloudflareClient;

    #[test]
    fn cf_time_string_matches_go_layout() {
        let s = CloudflareClient::cf_time_string();
        assert!(s.contains('T'));
        assert!(s.contains('.'));
        // Go: 2006-01-02T15:04:05.000-07:00
        assert!(
            s.len() >= 29,
            "expected timezone offset with colon, got {s:?}"
        );
        let tz_start = s.rfind(['+', '-']).expect("timezone offset");
        assert_eq!(&s[tz_start + 3..tz_start + 4], ":");
    }
}
