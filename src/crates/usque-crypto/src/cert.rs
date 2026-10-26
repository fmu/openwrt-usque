use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls_pki_types::PrivatePkcs8KeyDer;

use crate::keys::{private_key_pkcs8_der, SigningKey};

/// Build the self-signed client certificate the MASQUE endpoint authenticates
/// the device with.
///
/// `rcgen` is configured with the `ring` backend (see `usque-crypto`'s
/// `Cargo.toml`); `ring` requires the PKCS#8 v1 document to embed the public
/// key, which [`crate::keys`] guarantees for both generated and legacy SEC1
/// keys.
pub fn generate_self_signed_cert(signing_key: &SigningKey) -> Result<(Vec<u8>, Vec<u8>)> {
    let key_der = private_key_pkcs8_der(signing_key)?;
    let key_der = PrivatePkcs8KeyDer::from(key_der);
    let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &PKCS_ECDSA_P256_SHA256)
        .context("failed to build key pair")?;

    let mut params = CertificateParams::new(vec![]).context("failed to create cert params")?;
    // Fixed validity window: the certificate is pinned by public key, not by
    // its dates, and a long window avoids clock-skew failures on routers
    // whose clock has not been synchronised yet at boot.
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2030, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .context("failed to generate certificate")?;
    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    Ok((cert_der, key_der))
}

#[cfg(test)]
mod tests {
    use crate::keys::SigningKey;

    use super::generate_self_signed_cert;

    #[test]
    fn cert_der_is_a_certificate() {
        let key = SigningKey::generate().unwrap();
        let (cert_der, key_der) = generate_self_signed_cert(&key).unwrap();
        assert_eq!(key_der, key.pkcs8_der().to_vec());
        // The certificate wraps the identity key: its SPKI must match, and it
        // must be signed with ecdsa-with-SHA256 (OID 1.2.840.10045.4.3.2).
        let spki = crate::der::certificate_spki_der(&cert_der).unwrap();
        assert_eq!(spki, key.public_key_spki_der().as_slice());
        let ecdsa_sha256_oid = [0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
        assert!(
            cert_der.windows(ecdsa_sha256_oid.len()).any(|w| w == ecdsa_sha256_oid),
            "certificate must be signed with ecdsa-with-SHA256"
        );
    }
}
