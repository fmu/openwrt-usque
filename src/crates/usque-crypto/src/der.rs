//! Tiny DER (distinguished encoding rules) reader.
//!
//! Shared by the key parser (SEC1 `ECPrivateKey`, PKCS#8) and the endpoint
//! certificate pinning check. Only definite-length TLVs, as DER mandates.
//! Keeping this in-tree instead of pulling `x509-parser`/`asn1-rs`/`nom` saves
//! a few hundred kilobytes of `.text`, which the OpenWrt overlay cannot spare.

use anyhow::{bail, Context, Result};

pub(crate) struct Der<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Der<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Read one TLV, returning `(tag, value)`.
    fn read_tlv(&mut self) -> Result<(u8, &'a [u8])> {
        let tag = *self.data.get(self.pos).context("unexpected end of DER")?;
        let len_byte = *self
            .data
            .get(self.pos + 1)
            .context("truncated DER length")?;
        self.pos += 2;

        let len = if len_byte < 0x80 {
            len_byte as usize
        } else {
            let n = (len_byte & 0x7f) as usize;
            // 0 would be the BER indefinite form; >4 is absurd for our sizes.
            if n == 0 || n > 4 {
                bail!("unsupported DER length encoding 0x{len_byte:02x}");
            }
            let mut len = 0usize;
            for _ in 0..n {
                let byte = *self.data.get(self.pos).context("truncated DER length")?;
                self.pos += 1;
                len = (len << 8) | byte as usize;
            }
            len
        };

        let end = self.pos.checked_add(len).context("DER length overflow")?;
        let value = self
            .data
            .get(self.pos..end)
            .context("truncated DER value")?;
        self.pos = end;
        Ok((tag, value))
    }

    /// Read one TLV and require a specific tag, returning its value.
    pub(crate) fn expect(&mut self, tag: u8) -> Result<&'a [u8]> {
        let (actual, value) = self.read_tlv()?;
        if actual != tag {
            bail!("unexpected DER tag 0x{actual:02x} (expected 0x{tag:02x})");
        }
        Ok(value)
    }

    /// Read one TLV and return its complete encoding (`tag || len || value`).
    pub(crate) fn expect_raw(&mut self, tag: u8) -> Result<&'a [u8]> {
        let start = self.pos;
        let (actual, _) = self.read_tlv()?;
        if actual != tag {
            bail!("unexpected DER tag 0x{actual:02x} (expected 0x{tag:02x})");
        }
        self.data
            .get(start..self.pos)
            .context("internal DER span error")
    }
}

/// Extract the `SubjectPublicKeyInfo` DER from an X.509 certificate.
///
/// ```text
/// Certificate ::= SEQUENCE {
///   tbsCertificate       TBSCertificate,
///   signatureAlgorithm   AlgorithmIdentifier,
///   signatureValue       BIT STRING }
/// TBSCertificate ::= SEQUENCE {
///   version         [0] EXPLICIT Version DEFAULT v1,
///   serialNumber        CertificateSerialNumber,
///   signature           AlgorithmIdentifier,
///   issuer              Name,
///   validity            Validity,
///   subject             Name,
///   subjectPublicKeyInfo SubjectPublicKeyInfo,
///   ... }
/// ```
///
/// The SPKI is returned verbatim (including its SEQUENCE header) so it can be
/// compared byte-for-byte with the key from `config.json` — the same value
/// `x509-parser`'s `cert.public_key().raw` exposes.
pub(crate) fn certificate_spki_der(cert_der: &[u8]) -> Result<&[u8]> {
    let mut cert = Der::new(cert_der);
    let cert_body = cert
        .expect(0x30)
        .context("certificate is not a SEQUENCE")?;
    if !cert.is_empty() {
        bail!("trailing data after certificate");
    }

    let mut tbs = Der::new(cert_body);
    let tbs_body = tbs
        .expect(0x30)
        .context("certificate has no tbsCertificate")?;
    let mut tbs = Der::new(tbs_body);

    // version [0] EXPLICIT is optional (v1 certificates omit it).
    if tbs.peek() == Some(0xa0) {
        tbs.expect(0xa0).context("bad version field")?;
    }
    tbs.expect(0x02).context("missing serialNumber")?; // serialNumber
    tbs.expect(0x30).context("missing signature algorithm")?; // signature
    tbs.expect(0x30).context("missing issuer")?; // issuer
    tbs.expect(0x30).context("missing validity")?; // validity
    tbs.expect(0x30).context("missing subject")?; // subject
    tbs.expect_raw(0x30)
        .context("missing subjectPublicKeyInfo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spki_out_of_a_generated_certificate() {
        use crate::keys::SigningKey;

        let key = SigningKey::generate().unwrap();
        let (cert_der, _) = crate::cert::generate_self_signed_cert(&key).unwrap();
        let spki = certificate_spki_der(&cert_der).unwrap();
        assert_eq!(spki, key.public_key_spki_der().as_slice());
    }

    #[test]
    fn rejects_truncated_certificate() {
        assert!(certificate_spki_der(&[]).is_err());
        assert!(certificate_spki_der(&[0x30, 0x00]).is_err());
        assert!(certificate_spki_der(b"not a certificate").is_err());
    }

    #[test]
    fn reads_long_form_lengths() {
        // SEQUENCE (0x30) with 0x81 0x80 = 128 content bytes.
        let mut data = vec![0x30, 0x81, 0x80];
        data.extend(std::iter::repeat_n(0u8, 128));
        let mut der = Der::new(&data);
        assert_eq!(der.expect(0x30).unwrap().len(), 128);
        assert!(der.is_empty());
    }

    #[test]
    fn rejects_indefinite_length() {
        let mut der = Der::new(&[0x30, 0x80, 0x00, 0x00]);
        assert!(der.expect(0x30).is_err());
    }
}
