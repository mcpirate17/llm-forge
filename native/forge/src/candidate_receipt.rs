//! Candidate receipt digest verification. A valid digest is not an approval.
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

const MAX_BYTES: usize = 16 * 1024 * 1024;

pub struct VerifiedReceipt {
    pub payload: Value,
    pub receipt_id: String,
    pub decision: String,
    pub digest: String,
}

/// Verify a legacy receipt's self-consistency. Does not establish authorship or approval.
pub fn parse_and_verify(bytes: &[u8]) -> Result<VerifiedReceipt> {
    ensure!(bytes.len() <= MAX_BYTES, "candidate receipt exceeds 16 MiB");
    let text = std::str::from_utf8(bytes).context("candidate receipt must be UTF-8")?;
    let payload = crate::tool_schema::parse_document_with_limit(text, MAX_BYTES)
        .context("invalid candidate receipt JSON")?;
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
