use bytes::{BufMut, Bytes};

use crate::capsule::{put_varint, CAPSULE_CONNECT_IP_DATA};

const CONTEXT_ID: u64 = 0;

/// Decrement TTL/hop-limit in place on a valid IP packet.
pub fn decrement_ttl(packet: &mut [u8]) -> anyhow::Result<()> {
    if packet.is_empty() {
        anyhow::bail!("empty IP packet");
    }
    match packet[0] >> 4 {
        4 => {
            use etherparse::IpHeaders;
            let (mut headers, _) = IpHeaders::from_slice(packet)
                .map_err(|err| anyhow::anyhow!("failed to parse IPv4 packet: {err}"))?;
            let IpHeaders::Ipv4(header, _) = &mut headers else {
                anyhow::bail!("failed to decode IPv4 header");
            };
            if header.time_to_live <= 1 {
                anyhow::bail!("IPv4 TTL too small: {}", header.time_to_live);
            }
            header.time_to_live -= 1;
            let header_len = header.header_len();
            if packet.len() < header_len {
                anyhow::bail!("IPv4 header too short");
            }
            let mut cursor = std::io::Cursor::new(&mut packet[..header_len]);
            header
                .write(&mut cursor)
                .map_err(|err| anyhow::anyhow!("failed to write IPv4 header: {err}"))?;
        }
        6 => {
            if packet.len() < 40 {
                anyhow::bail!("IPv6 packet too short");
            }
            let hop = packet[7];
            if hop <= 1 {
                anyhow::bail!("IPv6 hop limit too small: {hop}");
            }
            packet[7] -= 1;
        }
        version => anyhow::bail!("unknown IP version: {version}"),
    }
    Ok(())
}

/// Encode the H3 datagram payload by writing the context-id varint
/// followed by the packet into a caller-owned buffer. Assumes
/// the caller has already decremented the IP TTL/hop-limit.
///
/// Generic over `BufMut` so the outbound path can encode directly into
/// the final `Vec<u8>` payload (no intermediate `BytesMut` + copy).
pub fn encode_h3_datagram_payload_into(packet: &[u8], out: &mut impl BufMut) -> anyhow::Result<()> {
    put_varint(out, CONTEXT_ID);
    out.put_slice(packet);
    Ok(())
}

/// Decode a CONNECT-IP datagram payload (after the quarter-stream-id
/// prefix has been stripped). Returns a `Bytes` slice that aliases the
/// provided source `Bytes` — zero copy.
pub fn decode_h3_datagram_payload_owned(data: &Bytes) -> Option<Bytes> {
    let (context, c_len) = decode_varint_local(data)?;
    if context != CONTEXT_ID {
        return None;
    }
    if data.len() < c_len {
        return None;
    }
    let payload = data.slice(c_len..);
    if payload.is_empty() {
        return None;
    }
    Some(payload)
}

/// Wrap an IP packet for HTTP/2 CONNECT-IP (DATAGRAM capsule type 0).
/// Assumes the caller has already decremented the IP TTL/hop-limit.
/// Generic over `BufMut`; see [`encode_h3_datagram_payload_into`].
pub fn encode_h2_datagram_capsule_into(
    packet: &[u8],
    out: &mut impl BufMut,
) -> anyhow::Result<()> {
    crate::capsule::put_capsule(out, CAPSULE_CONNECT_IP_DATA, packet);
    Ok(())
}

fn decode_varint_local(data: &Bytes) -> Option<(u64, usize)> {
    let first = *data.first()?;
    let len = 1usize << (first >> 6);
    if data.len() < len {
        return None;
    }
    let mut value = (first & 0x3f) as u64;
    for &byte in &data[1..len] {
        value = (value << 8) | byte as u64;
    }
    Some((value, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn h3_datagram_roundtrip() {
        let mut packet = vec![
            0x45, 0x00, 0x00, 0x14, 0x00, 0x00, 0x40, 0x00, 0x40, 0x06, 0x00, 0x00, 0x7f, 0x00,
            0x00, 0x01, 0x7f, 0x00, 0x00, 0x01,
        ];
        // Caller is responsible for the TTL decrement.
        decrement_ttl(&mut packet).unwrap();
        let mut out = BytesMut::new();
        encode_h3_datagram_payload_into(&packet, &mut out).unwrap();
        let decoded = decode_h3_datagram_payload_owned(&out.freeze()).unwrap();
        // TTL was decremented from 0x40 (64) to 0x3f (63).
        assert_eq!(decoded[8], 0x3f);
    }

    #[test]
    fn decode_zero_copy_aliases_input() {
        let mut packet = vec![
            0x45, 0x00, 0x00, 0x14, 0x00, 0x00, 0x40, 0x00, 0x40, 0x06, 0x00, 0x00, 0x7f, 0x00,
            0x00, 0x01, 0x7f, 0x00, 0x00, 0x01,
        ];
        decrement_ttl(&mut packet).unwrap();
        let mut encoded = BytesMut::new();
        encode_h3_datagram_payload_into(&packet, &mut encoded).unwrap();
        let encoded = encoded.freeze();
        let decoded = decode_h3_datagram_payload_owned(&encoded).unwrap();
        // Skip the varint prefix (1 byte for context id 0).
        let prefix = 1;
        assert_eq!(
            decoded.as_ptr(),
            unsafe { encoded.as_ptr().add(prefix) },
            "decoded Bytes must alias the encoded buffer"
        );
    }
}
