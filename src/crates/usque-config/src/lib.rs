mod endpoints;

pub use endpoints::{select_endpoint, EndpointSelection, DEFAULT_ENDPOINT_H2_V4};

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const HTTP2_WIKI_URL: &str = "https://github.com/Diniboy1123/usque/wiki/HTTP-2-support";

/// Application configuration compatible with the Go `config.json` format.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(rename = "private_key")]
    pub private_key: String,
    #[serde(rename = "endpoint_v4")]
    pub endpoint_v4: String,
    #[serde(rename = "endpoint_v6")]
    pub endpoint_v6: String,
    #[serde(rename = "endpoint_h2_v4", default)]
    pub endpoint_h2_v4: String,
    #[serde(rename = "endpoint_h2_v6", default)]
    pub endpoint_h2_v6: String,
    #[serde(rename = "endpoint_pub_key")]
    pub endpoint_pub_key: String,
    #[serde(default)]
    pub license: String,
    pub id: String,
    #[serde(rename = "access_token")]
    pub access_token: String,
    pub ipv4: String,
    pub ipv6: String,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())
            .with_context(|| format!("failed to open config file {}", path.as_ref().display()))?;
        let reader = BufReader::new(file);
        serde_json::from_reader(reader).context("failed to decode config file")
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let file = File::create(path.as_ref())
            .with_context(|| format!("failed to create config file {}", path.as_ref().display()))?;
        let writer = BufWriter::new(file);
        let mut ser = serde_json::Serializer::with_formatter(writer, PrettyFormatter::new());
        self.serialize(&mut ser)
            .context("failed to encode config file")?;
        Ok(())
    }
}

/// Pretty-print JSON with two-space indent matching Go output.
struct PrettyFormatter {
    current_indent: usize,
}

impl PrettyFormatter {
    fn new() -> Self {
        Self { current_indent: 0 }
    }
}

impl serde_json::ser::Formatter for PrettyFormatter {
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.current_indent += 1;
        writer.write_all(b"[")?;
        Ok(())
    }

    fn end_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.current_indent -= 1;
        writer.write_all(b"]")?;
        Ok(())
    }

    fn begin_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.current_indent += 1;
        if self.current_indent == 1 {
            writer.write_all(b"{\n")?;
        } else {
            writer.write_all(b"{")?;
        }
        Ok(())
    }

    fn end_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.current_indent -= 1;
        if self.current_indent == 0 {
            writer.write_all(b"\n}\n")?;
        } else {
            writer.write_all(b"}")?;
        }
        Ok(())
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if self.current_indent == 1 && !first {
            writer.write_all(b",\n")?;
        } else if !first {
            writer.write_all(b",")?;
        }
        if self.current_indent == 1 {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }

    fn begin_string<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(b"\"")?;
        Ok(())
    }

    fn end_string<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(b"\"")?;
        Ok(())
    }
}

/// Strip Cloudflare endpoint suffixes like `:0` or `[...]:0` from API responses.
pub fn parse_endpoint_v4(raw: &str) -> String {
    raw.strip_suffix(":0").unwrap_or(raw).to_string()
}

pub fn parse_endpoint_v6(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('[').trim_end_matches("]:0");
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoints() {
        assert_eq!(parse_endpoint_v4("162.159.198.1:0"), "162.159.198.1");
        assert_eq!(parse_endpoint_v6("[2606:4700:103::]:0"), "2606:4700:103::");
    }

    #[test]
    fn roundtrip_config() {
        let cfg = Config {
            private_key: "abc".into(),
            endpoint_v4: "1.2.3.4".into(),
            endpoint_v6: "::1".into(),
            endpoint_h2_v4: DEFAULT_ENDPOINT_H2_V4.into(),
            endpoint_h2_v6: String::new(),
            endpoint_pub_key: "pem".into(),
            license: "lic".into(),
            id: "id".into(),
            access_token: "tok".into(),
            ipv4: "100.96.0.1".into(),
            ipv6: "2606::1".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, loaded);
    }
}
