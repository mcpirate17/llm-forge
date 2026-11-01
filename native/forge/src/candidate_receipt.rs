//! Candidate receipt sealing and verification. A valid digest is not an approval.
//! Inputs are bounded to 16 MiB, shared-parser depth, 4300 integer digits,
//! and finite floating-point values. No executable interpreter is used.
#[path = "candidate_receipt/canonical.rs"]
mod canonical;
#[cfg(test)]
#[path = "candidate_receipt/tests.rs"]
mod tests;

use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_BYTES: usize = 16 * 1024 * 1024;

pub struct VerifiedReceipt {
    payload: Value,
    receipt_id: String,
    decision: String,
    digest: String,
}

impl VerifiedReceipt {
    /// Immutable digest-checked payload, including untrusted recorded evidence.
    pub fn payload(&self) -> &Value {
        &self.payload
    }
    /// Display-only identifier: legacy receipt digests deliberately exclude it.
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }
    pub fn decision(&self) -> &str {
        &self.decision
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

fn parse(bytes: &[u8]) -> Result<Value> {
    ensure!(bytes.len() <= MAX_BYTES, "candidate receipt exceeds 16 MiB");
    let text = std::str::from_utf8(bytes).context("candidate receipt must be UTF-8")?;
    let value = crate::tool_schema::parse_document_with_limit(text, MAX_BYTES)
        .context("invalid candidate receipt JSON")?;
    ensure!(value.is_object(), "candidate receipt must be an object");
    Ok(value)
}

/// Seal bounded JSON using the existing receipt byte contract. This generates
/// an integrity checksum, never a signature, execution proof or approval.
pub fn seal(bytes: &[u8]) -> Result<VerifiedReceipt> {
    let mut payload = parse(bytes)?;
    payload["receipt_id"] = Value::String(String::new());
    payload["receipt_digest"] = Value::String(String::new());
    let unsigned = canonical::encode(&payload)?;
    let digest = format!("{:x}", Sha256::digest(unsigned.as_bytes()));
    payload["receipt_id"] = Value::String(format!("gr-{}", &digest[..24]));
    payload["receipt_digest"] = Value::String(digest);
    // Reserve room for the generated fields as well as the submitted payload.
    crate::json_canon::encoded_size(&payload, MAX_BYTES)?;
    parse_and_verify(&serde_json::to_vec(&payload)?)
}

/// Verify a legacy receipt's self-consistency. Does not establish authorship or approval.
pub fn parse_and_verify(bytes: &[u8]) -> Result<VerifiedReceipt> {
    let payload = parse(bytes)?;
    let fields = payload
        .as_object()
        .context("candidate receipt must be an object")?;
    let digest = fields
        .get("receipt_digest")
        .and_then(Value::as_str)
        .context("candidate receipt digest missing")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "candidate receipt digest must be 64 lowercase hexadecimal characters"
    );
    let mut unsigned = payload.clone();
    let values = unsigned.as_object_mut().expect("validated object");
    values.insert("receipt_id".into(), Value::String(String::new()));
    values.insert("receipt_digest".into(), Value::String(String::new()));
    let canonical = canonical::encode(&unsigned)?;
    let actual = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    ensure!(actual == digest, "candidate receipt digest mismatch");
    Ok(VerifiedReceipt {
        receipt_id: fields
            .get("receipt_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        decision: fields
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        digest: digest.to_owned(),
        payload,
    })
}
