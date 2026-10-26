use anyhow::{bail, Context, Result};
use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use pem::parse;

/// The private key type used for enrollment and TLS identities.
pub type SigningKey = EcdsaKeyPair;

/// Endpoint public key, stored as X.509 `SubjectPublicKeyInfo` DER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPublicKey(Vec<u8>);

impl PeerPublicKey {
    pub fn as_der(&self) -> &[u8] {
        &self.0
    }

    pub fn into_der(self) -> Vec<u8> {
        self.0
    }
}

pub fn generate_ec_keypair() -> Result<(Vec<u8>, Vec<u8>)> {
    let signing_key = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING)
        .map_err(|_| anyhow::anyhow!("failed to generate EC key pair"))?;
    let private_der = signing_key
        .to_pkcs8v1()
        .context("failed to marshal private key")?
        .as_ref()
        .to_vec();
    let public_der = public_key_spki_der(&signing_key)?;
    Ok((private_der, public_der))
}

pub fn decode_private_key(b64: &str) -> Result<SigningKey> {
    let der = STANDARD
        .decode(b64.trim())
        .context("failed to decode private key")?;
    // `from_private_key_der` accepts PKCS#8 `PrivateKeyInfo` as well as
    // SEC1 `ECPrivateKey` (RFC 5915), which Go configs sometimes use.
    EcdsaKeyPair::from_private_key_der(&ECDSA_P256_SHA256_FIXED_SIGNING, &der)
        .map_err(|e| anyhow::anyhow!("failed to parse private key: {e}"))
}

pub fn encode_private_key(signing_key: &SigningKey) -> Result<String> {
    let der = signing_key
        .to_pkcs8v1()
        .context("failed to marshal private key")?;
    Ok(STANDARD.encode(der.as_ref()))
}

pub fn public_key_spki_der(signing_key: &SigningKey) -> Result<Vec<u8>> {
    let der = AsDer::<PublicKeyX509Der>::as_der(signing_key.public_key())
        .map_err(|_| anyhow::anyhow!("failed to marshal public key"))?;
    Ok(der.as_ref().to_vec())
}

pub fn private_key_pkcs8_der(signing_key: &SigningKey) -> Result<Vec<u8>> {
    let der = signing_key
        .to_pkcs8v1()
        .context("failed to marshal private key")?;
    Ok(der.as_ref().to_vec())
}

pub fn decode_endpoint_public_key(pem_str: &str) -> Result<PeerPublicKey> {
    let pem = parse(pem_str).context("failed to decode endpoint public key")?;
    if pem.tag() != "PUBLIC KEY" {
        bail!("expected PUBLIC KEY PEM block");
    }
    Ok(PeerPublicKey(pem.contents().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_roundtrip() {
        let (priv_der, pub_der) = generate_ec_keypair().unwrap();
        assert!(!priv_der.is_empty());
        assert!(!pub_der.is_empty());

        let encoded = STANDARD.encode(&priv_der);
        let decoded = decode_private_key(&encoded).unwrap();
        let reencoded = encode_private_key(&decoded).unwrap();
        assert_eq!(encoded, reencoded);
    }

    #[test]
    fn endpoint_public_key_from_pem() {
        let (_, pub_der) = generate_ec_keypair().unwrap();
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            STANDARD.encode(&pub_der)
        );
        decode_endpoint_public_key(&pem).unwrap();
    }
}
