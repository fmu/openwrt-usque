use std::sync::{Arc, Once};

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};

use crate::cert::generate_self_signed_cert;
use crate::keys::{PeerPublicKey, SigningKey};

static CRYPTO_PROVIDER: Once = Once::new();

/// Install the process-wide rustls `ring` backend. Safe to call multiple times.
///
/// `ring` (not `aws-lc-rs`) is used because `aws-lc-rs` cannot be built for
/// 32-bit ARM, which is the OpenWrt target. All rustls/quinn/rcgen users in
/// this workspace are configured for the `ring` provider.
pub fn init() {
    CRYPTO_PROVIDER.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("failed to install rustls ring crypto provider");
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

    let mut config = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("failed to configure TLS versions")?
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
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Pins the MASQUE endpoint by comparing the `SubjectPublicKeyInfo` inside the
/// server certificate with the key Cloudflare handed out at registration time.
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
        let spki = crate::der::certificate_spki_der(end_entity.as_ref()).map_err(|_| {
            RustlsError::General("failed to parse server certificate".into())
        })?;

        if spki != self.expected.as_der() {
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
    use base64::Engine as _;

    use super::*;

    #[test]
    fn init_and_build_config() {
        init();
        let signing_key = SigningKey::generate().unwrap();
        let peer_der = crate::keys::public_key_spki_der(&signing_key).unwrap();
        let peer_key = crate::keys::decode_endpoint_public_key(&format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&peer_der)
        ))
        .unwrap();

        let config = build_rustls_config(
            &signing_key,
            TlsOptions {
                sni: "example.com".into(),
                insecure: true,
                peer_public_key: peer_key,
                alpn: vec![b"h3".to_vec()],
            },
        )
        .unwrap();
        assert_eq!(config.alpn_protocols, vec![b"h3".to_vec()]);
    }

    /// The generated self-signed certificate must be signable by `rcgen` with
    /// the `ring` backend and produce a `PKCS#8 v1` private key that `ring`
    /// (and therefore `rustls`) accepts for client authentication.
    #[test]
    fn client_auth_material_is_ring_compatible() {
        init();
        let signing_key = SigningKey::generate().unwrap();
        let (cert_der, key_der) = crate::cert::generate_self_signed_cert(&signing_key).unwrap();
        assert!(!cert_der.is_empty());
        assert_eq!(key_der, signing_key.pkcs8_der().to_vec());

        let signing_key = rustls::crypto::ring::sign::any_ecdsa_type(&PrivateKeyDer::Pkcs8(
            key_der.into(),
        ))
        .expect("ring provider must parse our PKCS#8 v1 key");
        let signer = signing_key
            .choose_scheme(&[SignatureScheme::ECDSA_NISTP256_SHA256])
            .expect("P-256 scheme must be offered");
        let signature = signer.sign(b"hello").expect("signature must be produced");
        assert!(!signature.is_empty());
    }
}
