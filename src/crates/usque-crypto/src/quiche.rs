use std::path::PathBuf;

use anyhow::{Context, Result};
use pem::Pem;
use tempfile::TempDir;

use crate::cert::generate_self_signed_cert;
use crate::keys::SigningKey;

/// Client TLS material for quiche/tokio-quiche (PEM files on disk).
pub struct QuicheClientCredentials {
    _temp_dir: TempDir,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl std::fmt::Debug for QuicheClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicheClientCredentials")
            .field("cert_path", &self.cert_path)
            .field("key_path", &self.key_path)
            .finish()
    }
}

pub fn prepare_quiche_client_credentials(
    signing_key: &SigningKey,
) -> Result<QuicheClientCredentials> {
    let (cert_der, key_der) = generate_self_signed_cert(signing_key)?;
    let temp_dir = TempDir::new().context("failed to create TLS temp directory")?;
    let cert_path = temp_dir.path().join("cert.pem");
    let key_path = temp_dir.path().join("key.pem");

    let cert_pem = Pem::new("CERTIFICATE", cert_der);
    let key_pem = Pem::new("PRIVATE KEY", key_der);
    std::fs::write(&cert_path, cert_pem.to_string())
        .context("failed to write client certificate PEM")?;
    std::fs::write(&key_path, key_pem.to_string())
        .context("failed to write client private key PEM")?;

    Ok(QuicheClientCredentials {
        _temp_dir: temp_dir,
        cert_path,
        key_path,
    })
}
