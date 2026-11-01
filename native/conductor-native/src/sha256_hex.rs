//! Canonical SHA-256 identifiers for native configuration and manifests.
use serde::{Deserialize, Deserializer, Serializer};

pub fn parse(value: &str) -> Result<[u8; 32], &'static str> {
    if value.len() != 64 {
        return Err("SHA-256 requires 64 lowercase hexadecimal characters");
    }
    let mut output = [0u8; 32];
    for (slot, pair) in output.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let high = digit(pair[0]).ok_or("invalid lowercase SHA-256 hex digit")?;
        let low = digit(pair[1]).ok_or("invalid lowercase SHA-256 hex digit")?;
        *slot = high * 16 + low;
    }
    Ok(output)
}
fn digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}
pub fn serialize<S: Serializer>(value: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = [0u8; 64];
    for (slots, &value) in encoded.as_chunks_mut::<2>().0.iter_mut().zip(value) {
        slots[0] = DIGITS[(value >> 4) as usize];
        slots[1] = DIGITS[(value & 15) as usize];
    }
    serializer.serialize_str(std::str::from_utf8(&encoded).expect("ASCII hexadecimal digits"))
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Digest {
        #[serde(with = "super")]
        sha256: [u8; 32],
    }
    #[test]
    fn digest_json_is_canonical_and_roundtrips_all_bytes() {
        for byte in 0..=255 {
            let expected = Digest { sha256: [byte; 32] };
            let json = serde_json::to_string(&expected).unwrap();
            assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), expected);
            assert!(json.contains(&format!("{byte:02x}").repeat(32)));
        }
        for bad in [
            "".to_owned(),
            "0".repeat(63),
            "A".repeat(64),
            "g".repeat(64),
            "é".repeat(32),
        ] {
            assert!(parse(&bad).is_err());
        }
    }
}
