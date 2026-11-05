//! Typed independently supplied ordinary Cargo supporting-input binding.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportingInputs {
    pub schema_version: u32,
    pub algorithm: String,
    pub scope: String,
    pub digest: String,
    pub files: usize,
}
impl SupportingInputs {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.algorithm != "sha256-framed-path-content-v1"
            || self.scope != "cargo-root-rust-discovery-v1"
            || self.files < 2
            || self.files > 4096
            || self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid Cargo supporting-input identity".into());
        }
        Ok(())
    }
}
pub fn compare(
    recorded: Option<&serde_json::Value>,
    expected: Option<&SupportingInputs>,
) -> Result<(), String> {
    let expected = expected.ok_or("independently captured Cargo supporting inputs unavailable")?;
    expected.validate()?;
    let actual: SupportingInputs = serde_json::from_value(
        recorded
            .cloned()
            .ok_or("Cargo supporting-input identity missing")?,
    )
    .map_err(|_| "invalid Cargo supporting-input identity")?;
    actual.validate()?;
    if actual != *expected {
        return Err("Cargo supporting inputs changed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_stale_forged_or_untrusted_identity_refuses() {
        let expected = SupportingInputs {
            schema_version: 1,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "cargo-root-rust-discovery-v1".into(),
            digest: "a".repeat(64),
            files: 3,
        };
        let value = serde_json::to_value(&expected).unwrap();
        assert!(compare(Some(&value), Some(&expected)).is_ok());
        assert!(compare(None, Some(&expected)).is_err());
        assert!(compare(Some(&value), None).is_err());
        let mut changed = value.clone();
        changed["files"] = serde_json::json!(4);
        assert!(compare(Some(&changed), Some(&expected)).is_err());
        changed = value.clone();
        changed["digest"] = serde_json::json!("b".repeat(64));
        assert!(compare(Some(&changed), Some(&expected)).is_err());
        changed = value;
        changed["unknown"] = serde_json::json!(true);
        assert!(compare(Some(&changed), Some(&expected)).is_err());
    }
}
