//! Minimal HTTPS/1.1 JSON client for the Cloudflare WARP registration API.
//!
//! The device talks to `api.cloudflareclient.com` for exactly two calls
//! (register + enroll). Pulling in `reqwest`/`hyper`/`tower`/`url` for that
//! costs well over half a megabyte of `.text` on armv7, which the router
//! overlay cannot spare — so this module implements just what those two calls
//! need: TLS 1.2/1.3 via `rustls` + `tokio-rustls`, plain HTTP/1.1 with a
//! `Connection: close`, `Content-Length`/`chunked` response bodies, and a
//! compile-time Mozilla root store (`webpki-roots`) so the device needs no
//! `ca-bundle` package at runtime.
//!
//! Trust model: standard web PKI validation of the API endpoint (unlike the
//! MASQUE tunnel, which pins the endpoint's public key).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use usque_crypto::init as init_crypto;

/// Total wall-clock budget for one request (DNS + TCP + TLS + response).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Plenty for the API's responses; guards against unbounded buffering.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, Clone)]
pub struct HttpsClient {
    config: Arc<ClientConfig>,
}

impl HttpsClient {
    /// Build a client that validates servers against the bundled Mozilla root
    /// store. Callers should run [`usque_crypto::init`] first (done here too).
    pub fn new() -> Result<Self> {
        init_crypto();
        static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        if let Some(config) = CONFIG.get() {
            return Ok(Self {
                config: Arc::clone(config),
            });
        }

        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .context("failed to configure TLS versions")?
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        let _ = CONFIG.set(Arc::clone(&config));
        Ok(Self { config })
    }

    /// Perform one HTTPS request. `body` is sent verbatim (JSON, no encoding
    /// negotiation). The connection is closed after the response, so callers
    /// pay one handshake per API call — there are only ever two.
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let url = Url::parse(url)?;
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.request_inner(method, &url, headers, body),
        )
        .await
        .map_err(|_| anyhow::anyhow!("request to {url} timed out"))?
    }

    async fn request_inner(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse> {
        let tcp = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((url.host.as_str(), url.port)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("TCP connect to {}:{} timed out", url.host, url.port))?
        .with_context(|| format!("TCP connect to {}:{} failed", url.host, url.port))?;
        tcp.set_nodelay(true).ok();

        let server_name = ServerName::try_from(url.host.clone())
            .map_err(|_| anyhow::anyhow!("invalid server name {:?}", url.host))?;
        let mut tls = TlsConnector::from(Arc::clone(&self.config))
            .connect(server_name, tcp)
            .await
            .context("TLS handshake failed")?;

        let mut request = String::with_capacity(256 + body.map(|b| b.len()).unwrap_or(0));
        request.push_str(method);
        request.push(' ');
        request.push_str(&url.path);
        request.push_str(" HTTP/1.1\r\nHost: ");
        request.push_str(&url.host);
        request.push_str("\r\nConnection: close\r\nAccept: */*\r\n");
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("Content-Length: ");
        request.push_str(&body.map(|b| b.len()).unwrap_or(0).to_string());
        request.push_str("\r\n\r\n");

        tls.write_all(request.as_bytes())
            .await
            .context("failed to write request headers")?;
        if let Some(body) = body {
            tls.write_all(body)
                .await
                .context("failed to write request body")?;
        }
        tls.flush().await.context("failed to flush request")?;

        let mut conn = Conn::new(tls);
        let head = conn.read_head().await?;
        let head = parse_head(&head)?;
        let response_body = conn.read_body(&head).await?;
        Ok(HttpResponse {
            status: head.status,
            body: response_body,
        })
    }
}

#[derive(Debug, Clone)]
struct Url {
    host: String,
    port: u16,
    /// Path plus optional query string, always starting with `/`.
    path: String,
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "https://{}:{}{}", self.host, self.port, self.path)
    }
}

impl Url {
    fn parse(url: &str) -> Result<Self> {
        let rest = url
            .strip_prefix("https://")
            .with_context(|| format!("only https:// URLs are supported (got {url:?})"))?;
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(idx) => (&rest[..idx], rest[idx..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host.to_string(),
                port.parse::<u16>()
                    .with_context(|| format!("invalid port in {authority:?}"))?,
            ),
            None => (authority.to_string(), 443),
        };
        if host.is_empty() {
            bail!("missing host in URL {url:?}");
        }
        let path = path.split('#').next().unwrap_or("/");
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        Ok(Self { host, port, path })
    }
}

struct Head {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
}

struct Conn<S> {
    stream: S,
    buf: Vec<u8>,
    eof: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            buf: Vec::with_capacity(8 * 1024),
            eof: false,
        }
    }

    /// Append more bytes from the stream. Returns the number of bytes read;
    /// `0` marks EOF (recorded in `self.eof`).
    async fn fill(&mut self) -> Result<usize> {
        if self.eof {
            return Ok(0);
        }
        if self.buf.len() >= MAX_RESPONSE_BYTES {
            bail!("response exceeds {MAX_RESPONSE_BYTES} bytes");
        }
        let mut chunk = [0u8; 8192];
        let n = self
            .stream
            .read(&mut chunk)
            .await
            .context("failed to read response")?;
        if n == 0 {
            self.eof = true;
        } else {
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(n)
    }

    /// Read until the end of the response headers and return them.
    async fn read_head(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(idx) = find_subslice(&self.buf, b"\r\n\r\n") {
                let head = self.buf[..idx + 2].to_vec();
                self.buf.drain(..idx + 4);
                return Ok(head);
            }
            if self.buf.len() > 64 * 1024 {
                bail!("response headers too large");
            }
            if self.fill().await? == 0 {
                bail!("connection closed before response headers were complete");
            }
        }
    }

    async fn read_body(&mut self, head: &Head) -> Result<Vec<u8>> {
        if head.chunked {
            return self.read_chunked().await;
        }
        if let Some(len) = head.content_length {
            while self.buf.len() < len {
                if self.fill().await? == 0 {
                    bail!(
                        "connection closed before body was complete ({} of {len} bytes)",
                        self.buf.len()
                    );
                }
            }
            self.buf.truncate(len);
            return Ok(std::mem::take(&mut self.buf));
        }
        // No framing information: read until the server closes (we asked for
        // `Connection: close`).
        while self.fill().await? > 0 {}
        Ok(std::mem::take(&mut self.buf))
    }

    async fn read_chunked(&mut self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let line = self.read_line().await?;
            let size_str = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_str, 16)
                .with_context(|| format!("invalid chunk size {size_str:?}"))?;
            if size == 0 {
                // Consume trailers up to the terminating empty line.
                loop {
                    if self.read_line().await?.is_empty() {
                        break;
                    }
                }
                return Ok(body);
            }
            while self.buf.len() < size + 2 {
                if self.fill().await? == 0 {
                    bail!("connection closed mid-chunk");
                }
            }
            let trailer_ok = &self.buf[size..size + 2] == b"\r\n";
            if !trailer_ok {
                bail!("malformed chunked body (missing CRLF)");
            }
            body.extend_from_slice(&self.buf[..size]);
            self.buf.drain(..size + 2);
            if body.len() > MAX_RESPONSE_BYTES {
                bail!("response body exceeds {MAX_RESPONSE_BYTES} bytes");
            }
        }
    }

    /// Read one CRLF-terminated line (without the CRLF).
    async fn read_line(&mut self) -> Result<String> {
        loop {
            if let Some(idx) = find_subslice(&self.buf, b"\r\n") {
                let line = String::from_utf8_lossy(&self.buf[..idx]).into_owned();
                self.buf.drain(..idx + 2);
                return Ok(line);
            }
            if self.fill().await? == 0 {
                bail!("connection closed while reading a line");
            }
        }
    }
}

/// Parse the status line and headers (everything before the final CRLF).
fn parse_head(head: &[u8]) -> Result<Head> {
    let text = std::str::from_utf8(head).context("response headers are not valid UTF-8")?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .with_context(|| format!("malformed status line {status_line:?}"))?;

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .with_context(|| format!("invalid Content-Length {value:?}"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }

    Ok(Head {
        status,
        content_length,
        chunked,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn parse_url(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn parses_urls() {
        let url = parse_url("https://api.cloudflareclient.com/v0a4471/reg");
        assert_eq!(url.host, "api.cloudflareclient.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.path, "/v0a4471/reg");

        let url = parse_url("https://example.com:8443");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 8443);
        assert_eq!(url.path, "/");

        assert!(Url::parse("http://example.com/x").is_err());
        assert!(Url::parse("https://").is_err());
        assert!(Url::parse("https://example.com:notaport/x").is_err());
    }

    #[test]
    fn parses_status_and_headers() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 3\r\n\r\n";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.content_length, Some(3));
        assert!(!parsed.chunked);
    }

    #[test]
    fn parses_chunked_transfer_encoding() {
        let head = b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\n\r\n";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.status, 400);
        assert!(parsed.chunked);
        assert_eq!(parsed.content_length, None);
    }

    #[test]
    fn rejects_malformed_status_line() {
        assert!(parse_head(b"garbage\r\n\r\n").is_err());
    }

    /// Feed a canned HTTP response through the incremental reader (the
    /// `Conn` is generic over the stream so tests need no TLS).
    async fn read_canned_response(response: &[u8]) -> (u16, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        server.write_all(response).await.unwrap();
        // Close the write half: the EOF-framed response path needs it, and it
        // keeps the other tests from blocking on a live in-memory peer.
        drop(server);
        let mut conn = Conn::new(client);
        let head = conn.read_head().await.unwrap();
        let head = parse_head(&head).unwrap();
        let status = head.status;
        let body = conn.read_body(&head).await.unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn reads_chunked_body() {
        let (status, body) = read_canned_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n",
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, b"Wikipedia");
    }

    #[tokio::test]
    async fn reads_content_length_body() {
        let (status, body) = read_canned_response(
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 11\r\n\r\nhello world",
        )
        .await;
        assert_eq!(status, 429);
        assert_eq!(body, b"hello world");
    }

    #[tokio::test]
    async fn reads_until_eof_without_framing() {
        let (_, body) = read_canned_response(b"HTTP/1.1 200 OK\r\n\r\n{\"a\":1}").await;
        assert_eq!(body, b"{\"a\":1}");
    }

    #[tokio::test]
    async fn rejects_truncated_body() {
        let (client, mut server) = tokio::io::duplex(1024);
        server
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort")
            .await
            .unwrap();
        drop(server);
        let mut conn = Conn::new(client);
        let head = conn.read_head().await.unwrap();
        let head = parse_head(&head).unwrap();
        assert!(conn.read_body(&head).await.is_err());
    }
}
