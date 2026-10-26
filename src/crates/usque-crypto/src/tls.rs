use std::sync::{Arc, Once};

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};

use crate::cert::generate_self_signed_cert;
use crate::keys::{PeerPublicKey, SigningKey};

static CRYPTO_PROVIDER: Once = Once::new();

/// Install the process-wide rustls `aws-lc-rs` backend. Safe to call multiple times.
pub fn init() {
    CRYPTO_PROVIDER.call_once(|| {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("failed to install rustls aws-lc-rs crypto provider");
    });
}

pub struct TlsOptions {
    pub sni: String,
    pub insecure: bool,
    pub peer_public_key: PeerPublicKey,
    pub alpn: Vec<Vec<u8>>,
}

pub fn build_rustls_config(
    signing_key: &SigningKey,
    options: TlsOptions,
) -> Result<Arc<ClientConfig>> {
    init();
    let (cert_der, key_der) = generate_self_signed_cert(signing_key)?;
    let cert_chain = vec![CertificateDer::from(cert_der)];
    let private_key = PrivateKeyDer::Pkcs8(key_der.into());

    let verifier: Arc<dyn ServerCertVerifier> = if options.insecure {
        Arc::new(InsecureVerifier)
    } else {
        Arc::new(PinnedKeyVerifier {
            expected: options.peer_public_key.clone(),
        })
    };

    let mut config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(cert_chain, private_key)
        .context("failed to build TLS config")?;

    config.alpn_protocols = options.alpn;
    config.enable_sni = true;
    let _ = options.sni; // SNI is set by callers via ServerName at connect time
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct InsecureVerifier;

impl ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

#[derive(Debug)]
struct PinnedKeyVerifier {
    expected: PeerPublicKey,
}

impl ServerCertVerifier for PinnedKeyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let (_, cert) = x509_parser::parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| RustlsError::General("failed to parse server certificate".into()))?;

        let spki = cert.public_key();
        if spki.raw != self.expected.as_der() {
            return Err(RustlsError::General(
                "remote endpoint has a different public key than what we trust in config".into(),
            ));
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
        ]
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use base64::Engine as _;

    use super::*;

    #[test]
    fn init_and_build_config() {
        init();
        let signing_key = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
        let peer_der = crate::keys::public_key_spki_der(&signing_key).unwrap();
        let peer_key = crate::keys::decode_endpoint_public_key(&format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&peer_der)
        ))
        .unwrap();

        build_rustls_config(
            &signing_key,
            TlsOptions {
                sni: "example.com".into(),
                insecure: true,
                peer_public_key: peer_key,
                alpn: vec![b"h3".to_vec()],
            },
        )
        .unwrap();
    }
}
