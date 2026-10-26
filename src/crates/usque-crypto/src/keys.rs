//! P-256 identity keys for enrollment and the TLS client certificate.
//!
//! The Cloudflare WARP MASQUE API expects the device public key as a
//! `SubjectPublicKeyInfo` DER blob (base64) and the TLS client identity as a
//! self-signed certificate carrying the same P-256 key, signed with
//! `ecdsa_secp256r1_sha256`.
//!
//! This module is built on `ring` (not `aws-lc-rs`) because `aws-lc-rs` has no
//! 32-bit ARM support and the OpenWrt target is `armv7`.
//!
//! `ring` deliberately hides the private scalar, so a [`SigningKey`] keeps the
//! canonical PKCS#8 v1 DER document it was created/parsed from. `ring`'s
//! `EcdsaKeyPair::from_pkcs8` only accepts PKCS#8 v1 whose inner
//! `ECPrivateKey` (RFC 5915) embeds `[1] publicKey`; the Go client that
//! produced existing `config.json` files stores the *bare* `ECPrivateKey`
//! (SEC1) instead, which `from_private_key_and_public_key` can ingest as long
//! as the `[1] publicKey` field is present (Go always writes it).

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use pem::parse;

use crate::der::Der;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

/// TLS signature scheme `ecdsa_secp256r1_sha256` requires ASN.1/DER encoded
/// ECDSA signatures, hence the `..._ASN1_SIGNING` variant.
const SIGNING_ALG: &ring::signature::EcdsaSigningAlgorithm = &ECDSA_P256_SHA256_ASN1_SIGNING;

const P256_SCALAR_LEN: usize = 32;
const P256_POINT_LEN: usize = 65;

/// Prefix of a P-256 `SubjectPublicKeyInfo`:
///
/// ```text
/// SEQUENCE {                                  30 59
///   SEQUENCE { OID id-ecPublicKey,            30 13 06 07 2a8648ce3d0201
///              OID prime256v1 }               06 08 2a8648ce3d030107
///   BIT STRING { 0 unused bits, 0x04 || X || Y }  03 42 00
/// }
/// ```
///
/// The 65-byte uncompressed point (`ring`'s `public_key()`) is appended to
/// this prefix to build the 91-byte SPKI DER the API and TLS need.
const SPKI_P256_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
    0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// Prefix of a PKCS#8 v1 `PrivateKeyInfo` whose `privateKey` OCTET STRING
/// holds a 121-byte RFC 5915 `ECPrivateKey` (see [`build_pkcs8_v1`]):
///
/// ```text
/// SEQUENCE {                                        30 81 93
///   INTEGER 0 (v1)                                   02 01 00
///   SEQUENCE { OID id-ecPublicKey, prime256v1 }       30 13 06 07 2a8648ce3d0201 06 08 2a8648ce3d030107
///   OCTET STRING { ECPrivateKey (121 bytes) }         04 79
/// }
/// ```
const PKCS8_V1_PREFIX: [u8; 29] = [
    0x30, 0x81, 0x93, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
    0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x04, 0x79,
];

/// Start of an RFC 5915 `ECPrivateKey`: SEQUENCE header, version 1 and the
/// `privateKey` OCTET STRING header (32 bytes). The scalar follows.
const EC_PRIVATE_KEY_SCALAR_HEAD: [u8; 7] = [0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];

/// Tail of the `ECPrivateKey` after the scalar: the optional-but-conventional
/// `parameters [0]` named curve and the mandatory-for-`ring` `publicKey [1]`
/// BIT STRING header (0 unused bits, uncompressed point). The 65-byte point
/// follows.
const EC_PRIVATE_KEY_POINT_HEAD: [u8; 17] = [
    0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0xa1, 0x44, 0x03, 0x42,
    0x00,
];

const P256_CURVE_OID: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// The private key type used for enrollment and TLS identities.
pub struct SigningKey {
    key: EcdsaKeyPair,
    /// Canonical PKCS#8 v1 DER this key was created/parsed from.
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never format key material; identify the key by its public point.
        f.debug_struct("SigningKey")
            .field("public_key", &hex_encode(self.public_key_point()))
            .finish()
    }
}

impl SigningKey {
    /// Generate a fresh P-256 key pair, serialized as PKCS#8 v1 (with the
    /// public key embedded, as `ring` requires).
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(SIGNING_ALG, &rng)
            .map_err(|_| anyhow::anyhow!("failed to generate EC key pair"))?;
        Self::from_pkcs8_der(pkcs8.as_ref())
    }

    /// Parse an unencrypted PKCS#8 DER document (v1 `OneAsymmetricKey`).
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self> {
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(SIGNING_ALG, der, &rng)
            .map_err(|err| anyhow::anyhow!("failed to parse private key: {err:?}"))?;
        if key.public_key().as_ref().len() != P256_POINT_LEN {
            bail!("private key is not a P-256 key");
        }
        Ok(Self {
            key,
            pkcs8: der.to_vec(),
        })
    }

    /// The private key in PKCS#8 v1 DER form. `ring` requires the embedded
    /// public key, so this is also what `rcgen`/`rustls` are handed.
    pub fn pkcs8_der(&self) -> &[u8] {
        &self.pkcs8
    }

    /// The 65-byte uncompressed SEC1 public key point (`0x04 || X || Y`).
    pub fn public_key_point(&self) -> &[u8] {
        self.key.public_key().as_ref()
    }

    /// The 91-byte `SubjectPublicKeyInfo` DER for this key.
    pub fn public_key_spki_der(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SPKI_P256_PREFIX.len() + P256_POINT_LEN);
        out.extend_from_slice(&SPKI_P256_PREFIX);
        out.extend_from_slice(self.public_key_point());
        out
    }
}

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
    let key = SigningKey::generate()?;
    Ok((key.pkcs8_der().to_vec(), key.public_key_spki_der()))
}

/// Decode the base64 private key out of a `config.json`.
///
/// Accepts both encodings produced in the wild:
/// * PKCS#8 v1 `OneAsymmetricKey` (what this implementation writes, and what
///   `ring`/`aws-lc-rs` based builds wrote before), and
/// * bare SEC1 `ECPrivateKey` (RFC 5915), which is what the Go client — and
///   therefore every existing router config — writes.
pub fn decode_private_key(b64: &str) -> Result<SigningKey> {
    let der = STANDARD
        .decode(b64.trim())
        .context("failed to decode private key: invalid base64")?;

    match SigningKey::from_pkcs8_der(&der) {
        Ok(key) => Ok(key),
        Err(pkcs8_err) => decode_sec1(&der).map_err(|sec1_err| {
            anyhow::anyhow!(
                "failed to parse private key as PKCS#8 ({pkcs8_err}) or SEC1 ECPrivateKey ({sec1_err})"
            )
        }),
    }
}

/// Parse a bare SEC1 `ECPrivateKey` and re-wrap it as PKCS#8 v1.
///
/// `ring` cannot derive the public key from the scalar, so the SEC1 document
/// must carry `[1] publicKey` — Go always writes it, and
/// `EcdsaKeyPair::from_private_key_and_public_key` verifies that scalar and
/// point match.
fn decode_sec1(der: &[u8]) -> Result<SigningKey> {
    let (scalar, point) = parse_sec1(der)?;

    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_private_key_and_public_key(SIGNING_ALG, &scalar, point, &rng)
        .map_err(|err| anyhow::anyhow!("invalid EC private key: {err:?}"))?;

    Ok(SigningKey {
        key,
        pkcs8: build_pkcs8_v1(&scalar, point),
    })
}

/// Encode a signing key back to the config format (base64 PKCS#8 v1).
pub fn encode_private_key(signing_key: &SigningKey) -> Result<String> {
    Ok(STANDARD.encode(signing_key.pkcs8_der()))
}

pub fn public_key_spki_der(signing_key: &SigningKey) -> Result<Vec<u8>> {
    Ok(signing_key.public_key_spki_der())
}

pub fn private_key_pkcs8_der(signing_key: &SigningKey) -> Result<Vec<u8>> {
    Ok(signing_key.pkcs8_der().to_vec())
}

pub fn decode_endpoint_public_key(pem_str: &str) -> Result<PeerPublicKey> {
    let pem = parse(pem_str).context("failed to decode endpoint public key")?;
    if pem.tag() != "PUBLIC KEY" {
        bail!("expected PUBLIC KEY PEM block");
    }
    Ok(PeerPublicKey(pem.contents().to_vec()))
}

/// Fill a buffer with cryptographically secure random bytes (`ring`'s system
/// RNG; avoids pulling in `rand`/`getrandom` for the API client's serials).
pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    use ring::rand::SecureRandom as _;
    SystemRandom::new()
        .fill(buf)
        .map_err(|_| anyhow::anyhow!("failed to generate random bytes"))
}

/// Build a PKCS#8 v1 document around an RFC 5915 `ECPrivateKey` that embeds
/// the public key (`ring` rejects documents without it).
fn build_pkcs8_v1(scalar: &[u8], point: &[u8]) -> Vec<u8> {
    debug_assert_eq!(scalar.len(), P256_SCALAR_LEN);
    debug_assert_eq!(point.len(), P256_POINT_LEN);
    let mut out = Vec::with_capacity(150);
    out.extend_from_slice(&PKCS8_V1_PREFIX);
    out.extend_from_slice(&EC_PRIVATE_KEY_SCALAR_HEAD);
    out.extend_from_slice(scalar);
    out.extend_from_slice(&EC_PRIVATE_KEY_POINT_HEAD);
    out.extend_from_slice(point);
    debug_assert_eq!(out.len(), 150);
    out
}

/// Extract `(privateKey, publicKey)` from a SEC1 `ECPrivateKey` DER document.
fn parse_sec1(der: &[u8]) -> Result<(Vec<u8>, &[u8])> {
    let mut outer = Der::new(der);
    let body = outer.expect(0x30).context("ECPrivateKey: not a SEQUENCE")?;
    if !outer.is_empty() {
        bail!("ECPrivateKey: trailing data");
    }

    let mut seq = Der::new(body);
    let version = seq.expect(0x02).context("ECPrivateKey: missing version")?;
    if version != [1] {
        bail!("ECPrivateKey: unsupported version {version:?}");
    }

    let key = seq
        .expect(0x04)
        .context("ECPrivateKey: missing privateKey OCTET STRING")?;
    if key.is_empty() || key.len() > P256_SCALAR_LEN {
        bail!("ECPrivateKey: bad private key length {}", key.len());
    }
    // RFC 5915 allows a shorter octet string; `ring` wants it left-padded to
    // the fixed curve length.
    let mut scalar = vec![0u8; P256_SCALAR_LEN];
    scalar[P256_SCALAR_LEN - key.len()..].copy_from_slice(key);

    // [0] parameters: must be the named curve prime256v1 if present.
    if seq.peek() == Some(0xa0) {
        let params = seq.expect(0xa0).context("ECPrivateKey: bad parameters")?;
        let mut p = Der::new(params);
        let oid = p
            .expect(0x06)
            .context("ECPrivateKey: parameters not an OID")?;
        if oid != P256_CURVE_OID {
            bail!("ECPrivateKey: unsupported curve OID {oid:02x?}");
        }
        if !p.is_empty() {
            bail!("ECPrivateKey: trailing parameter data");
        }
    }

    // [1] publicKey: required, `ring` cannot derive it from the scalar.
    let point = if seq.peek() == Some(0xa1) {
        let wrapper = seq.expect(0xa1).context("ECPrivateKey: bad publicKey")?;
        let mut p = Der::new(wrapper);
        let bits = p
            .expect(0x03)
            .context("ECPrivateKey: publicKey not a BIT STRING")?;
        if !p.is_empty() {
            bail!("ECPrivateKey: trailing public key data");
        }
        // BIT STRING: first byte is the number of unused bits.
        let (unused, point) = bits
            .split_first()
            .context("ECPrivateKey: empty publicKey BIT STRING")?;
        if *unused != 0 {
            bail!("ECPrivateKey: public key has {unused} unused bits");
        }
        if point.len() != P256_POINT_LEN || point[0] != 0x04 {
            bail!(
                "ECPrivateKey: public key is not an uncompressed P-256 point ({} bytes)",
                point.len()
            );
        }
        point
    } else {
        bail!(
            "ECPrivateKey has no embedded public key; regenerate the config \
             (`usque enroll --regen-key`) or restore a Go-generated config"
        );
    };

    if !seq.is_empty() {
        bail!("ECPrivateKey: unexpected trailing fields");
    }

    Ok((scalar, point))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::Der;

    /// Pull the 32-byte scalar and 65-byte point out of a PKCS#8 v1 document
    /// produced by [`build_pkcs8_v1`], then rebuild the SEC1 `ECPrivateKey`
    /// layout the Go client writes (121 bytes, with `[0]` parameters).
    fn sec1_from_pkcs8(pkcs8: &[u8], point: &[u8]) -> Vec<u8> {
        let mut outer = Der::new(pkcs8);
        let body = outer.expect(0x30).unwrap();
        let mut seq = Der::new(body);
        assert_eq!(seq.expect(0x02).unwrap(), [0]);
        let _alg = seq.expect(0x30).unwrap();
        let ec_private_key = seq.expect(0x04).unwrap();

        let mut inner = Der::new(ec_private_key);
        let inner_body = inner.expect(0x30).unwrap();
        let mut ec = Der::new(inner_body);
        assert_eq!(ec.expect(0x02).unwrap(), [1]);
        let scalar = ec.expect(0x04).unwrap();
        assert_eq!(scalar.len(), P256_SCALAR_LEN);

        let mut out = EC_PRIVATE_KEY_SCALAR_HEAD.to_vec();
        out.extend_from_slice(scalar);
        out.extend_from_slice(&EC_PRIVATE_KEY_POINT_HEAD);
        out.extend_from_slice(point);
        out
    }

    #[test]
    fn generated_key_roundtrips() {
        let (priv_der, pub_der) = generate_ec_keypair().unwrap();
        // `ring` emits PKCS#8 v1 without the optional `[0]` parameters field.
        assert_eq!(priv_der.len(), 138);
        assert_eq!(pub_der.len(), 91);
        assert_eq!(pub_der[..26], SPKI_P256_PREFIX);
        assert_eq!(pub_der[26], 0x04);

        let encoded = STANDARD.encode(&priv_der);
        let decoded = decode_private_key(&encoded).unwrap();
        assert_eq!(encode_private_key(&decoded).unwrap(), encoded);
        assert_eq!(public_key_spki_der(&decoded).unwrap(), pub_der);
        assert_eq!(private_key_pkcs8_der(&decoded).unwrap(), priv_der);
    }

    #[test]
    fn generated_key_parses_as_pkcs8() {
        let key = SigningKey::generate().unwrap();
        let reparsed = SigningKey::from_pkcs8_der(key.pkcs8_der()).unwrap();
        assert_eq!(reparsed.public_key_spki_der(), key.public_key_spki_der());
    }

    #[test]
    fn spki_prefix_is_valid_der() {
        let key = SigningKey::generate().unwrap();
        let spki = key.public_key_spki_der();
        // Outer SEQUENCE with length 0x59 = 89 = 21 (alg id) + 68 (bit string).
        assert_eq!(&spki[..2], &[0x30, 0x59]);
        // Algorithm identifier: id-ecPublicKey + prime256v1.
        assert_eq!(&spki[2..4], &[0x30, 0x13]);
        assert_eq!(&spki[4..6], &[0x06, 0x07]);
        // Bit string with 0 unused bits and uncompressed point marker.
        assert_eq!(&spki[23..26], &[0x03, 0x42, 0x00]);
        assert_eq!(spki[26], 0x04);
    }

    /// The Go client's config format: a 121-byte SEC1 `ECPrivateKey` whose
    /// `[0]`/`[1]` fields are both present must load and produce the same
    /// identity as the equivalent PKCS#8 document.
    #[test]
    fn decodes_go_sec1_private_key() {
        let key = SigningKey::generate().unwrap();
        let sec1 = sec1_from_pkcs8(key.pkcs8_der(), key.public_key_point());
        assert_eq!(sec1.len(), 121);
        assert_eq!(sec1, sec1_from_pkcs8(key.pkcs8_der(), key.public_key_point()));

        let decoded = decode_private_key(&STANDARD.encode(&sec1)).unwrap();
        assert_eq!(decoded.public_key_spki_der(), key.public_key_spki_der());
        // Re-encoded as PKCS#8 v1 and stable across round-trips.
        let encoded = encode_private_key(&decoded).unwrap();
        let again = decode_private_key(&encoded).unwrap();
        assert_eq!(encode_private_key(&again).unwrap(), encoded);
        // Re-wrapped as PKCS#8 v1 with `[0]` parameters + `[1]` public key.
        assert_eq!(private_key_pkcs8_der(&decoded).unwrap().len(), 150);
    }

    /// `ring`'s own `generate_pkcs8` output omits the `[0]` parameters field
    /// of the inner `ECPrivateKey`; such documents must decode too.
    #[test]
    fn decodes_sec1_without_parameters() {
        let key = SigningKey::generate().unwrap();
        let full = sec1_from_pkcs8(key.pkcs8_der(), key.public_key_point());

        let mut body = Vec::with_capacity(109);
        // SEQUENCE header, version, privateKey OCTET STRING header.
        body.extend_from_slice(&full[..7]);
        body.extend_from_slice(&full[7..7 + P256_SCALAR_LEN]);
        // [1] publicKey only, no [0] parameters (the `ring`-generated layout).
        body.extend_from_slice(&[0xa1, 0x44, 0x03, 0x42, 0x00]);
        body.extend_from_slice(key.public_key_point());
        assert_eq!(body.len(), 109);
        body[1] = (body.len() - 2) as u8;

        let decoded = decode_private_key(&STANDARD.encode(&body)).unwrap();
        assert_eq!(decoded.public_key_spki_der(), key.public_key_spki_der());
    }

    #[test]
    fn rejects_sec1_without_public_key() {
        let key = SigningKey::generate().unwrap();
        let full = sec1_from_pkcs8(key.pkcs8_der(), key.public_key_point());
        // Drop the trailing [1] publicKey field and fix up the lengths.
        let mut body = full[..full.len() - 70].to_vec();
        assert_eq!(body.len(), 51);
        body[1] = (body.len() - 2) as u8; // 0x31

        let err = decode_private_key(&STANDARD.encode(&body)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no embedded public key"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn rejects_garbage_private_key() {
        assert!(decode_private_key("not base64!").is_err());
        assert!(decode_private_key(&STANDARD.encode([0x30, 0x00])).is_err());
    }

    #[test]
    fn endpoint_public_key_from_pem() {
        let key = SigningKey::generate().unwrap();
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            STANDARD.encode(key.public_key_spki_der())
        );
        let peer = decode_endpoint_public_key(&pem).unwrap();
        assert_eq!(peer.as_der(), key.public_key_spki_der());
    }

    #[test]
    fn fill_random_produces_varying_bytes() {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        fill_random(&mut a).unwrap();
        fill_random(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
