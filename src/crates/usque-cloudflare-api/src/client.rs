use std::io::{self, IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::http::HttpsClient;
use crate::models::{AccountData, ApiErrorBody, DeviceUpdate, ErrorInfo, Registration};
use crate::{
    API_URL, API_VERSION, DEFAULT_LOCALE, DEFAULT_MODEL, KEY_TYPE_MASQUE, KEY_TYPE_WG,
    TUN_TYPE_MASQUE, TUN_TYPE_WG,
};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub body: ApiErrorBody,
    pub status: u16,
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

impl ApiError {
    fn transport(status: u16, message: impl ToString) -> Self {
        Self {
            body: ApiErrorBody {
                success: false,
                errors: vec![ErrorInfo {
                    code: 0,
                    message: message.to_string(),
                }],
            },
            status,
        }
    }

    /// Map a non-2xx response body to an [`ApiErrorBody`], falling back to a
    /// transport-style error when the body is not the expected JSON.
    fn from_response(status: u16, body: &[u8]) -> Self {
        match serde_json::from_slice::<ApiErrorBody>(body) {
            Ok(body) => Self { body, status },
            Err(err) => Self::transport(status, err),
        }
    }
}

pub struct CloudflareClient {
    http: HttpsClient,
}

impl CloudflareClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: HttpsClient::new()?,
        })
    }

    /// Headers the Android WARP client sends; the API is picky about the
    /// user agent and client version.
    fn default_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("User-Agent", "WARP for Android"),
            ("CF-Client-Version", "a-6.35-4471"),
            ("Content-Type", "application/json; charset=UTF-8"),
            ("Connection", "Keep-Alive"),
        ]
    }

    /// Timestamp in the Go client's layout: `2006-01-02T15:04:05.000-07:00`.
    ///
    /// The Go client emits local time; this implementation always emits UTC
    /// (`+00:00`), which is the same RFC 3339 instant and is accepted by the
    /// API. It saves linking `chrono`/`iana-time-zone` for one timestamp.
    pub fn cf_time_string() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs() as i64;
        let millis = now.subsec_millis();
        let days = secs.div_euclid(86_400);
        let secs_of_day = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}+00:00",
            secs_of_day / 3600,
            (secs_of_day % 3600) / 60,
            secs_of_day % 60
        )
    }
}

/// Convert days since the Unix epoch to a proleptic Gregorian date
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
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
    usque_crypto::fill_random(&mut key)?;
    Ok(STANDARD.encode(key))
}

pub fn generate_random_android_serial() -> Result<String> {
    let mut serial = [0u8; 8];
    usque_crypto::fill_random(&mut serial)?;
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

    if !accept_tos {
        if !io::stdin().is_terminal() {
            bail!(
                "you must accept the Terms of Service (https://www.cloudflare.com/application/terms/) \
                 to register; pass --accept-tos for non-interactive use"
            );
        }
        if !prompt_yes_no(
            "You must accept the Terms of Service (https://www.cloudflare.com/application/terms/) to register. Do you agree? (y/n): ",
        )? {
            bail!("user did not accept TOS");
        }
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
    let body = serde_json::to_vec(&data).context("failed to encode registration request")?;

    let url = format!("{API_URL}/{API_VERSION}/reg");
    let mut headers = CloudflareClient::default_headers();
    if let Some(jwt) = jwt.filter(|s| !s.is_empty()) {
        headers.push(("CF-Access-Jwt-Assertion", jwt));
    }

    let resp = client
        .http
        .request("POST", &url, &headers, Some(&body))
        .await
        .context("failed to send registration request")?;

    if !resp.is_success() {
        let text = resp.text();
        if text.is_empty() {
            bail!("failed to register: HTTP {}", resp.status);
        }
        bail!("failed to register: HTTP {}: {text}", resp.status);
    }

    serde_json::from_slice::<AccountData>(&resp.body)
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
    let body = serde_json::to_vec(&update)
        .map_err(|err| ApiError::transport(0, format!("failed to encode request: {err}")))?;

    let url = format!("{API_URL}/{API_VERSION}/reg/{device_id}");
    let authorization = format!("Bearer {access_token}");
    let mut headers = CloudflareClient::default_headers();
    headers.push(("Authorization", authorization.as_str()));

    let resp = client
        .http
        .request("PATCH", &url, &headers, Some(&body))
        .await
        .map_err(|err| ApiError::transport(0, format!("{err:#}")))?;

    if !resp.is_success() {
        return Err(ApiError::from_response(resp.status, &resp.body));
    }

    serde_json::from_slice::<AccountData>(&resp.body)
        .map_err(|err| ApiError::transport(resp.status, format!("invalid response: {err}")))
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, CloudflareClient};

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
        assert_eq!(&s[tz_start..], "+00:00");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        // 2000-02-29 (leap day in a century leap year).
        let days = 11016;
        assert_eq!(civil_from_days(days), (2000, 2, 29));
        // 2026-10-26
        let days = 20752;
        assert_eq!(civil_from_days(days), (2026, 10, 26));
    }
}
