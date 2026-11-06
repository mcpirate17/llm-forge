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
        if !matches!(
            (self.schema_version, self.scope.as_str()),
            (1, "cargo-root-rust-discovery-v1") | (2, "complete-snapshot-inputs-v2")
        ) || self.algorithm != "sha256-framed-path-content-v1"
            || self.files < 2
            || self.files
                > if self.schema_version == 1 {
                    4096
                } else {
                    100_000
                }
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

/// Whether an independently verified profile uses the full snapshot domain.
pub fn uses_snapshot_inputs(profile: Option<&str>) -> bool {
    crate::mutation_workspace_selector::is_workspace(profile)
        || matches!(profile, Some("fest-confined-v1" | "mull-confined-v1"))
}

pub fn compare_profile(
    recorded: Option<&serde_json::Value>,
    expected: Option<&SupportingInputs>,
    profile: Option<&str>,
) -> Result<(), String> {
    let scope = match profile {
        None => "cargo-root-rust-discovery-v1",
        Some(profile) if uses_snapshot_inputs(Some(profile)) => "complete-snapshot-inputs-v2",
        Some(_) => return Err("unsupported verified execution profile".into()),
    };
    if expected.is_some_and(|identity| identity.scope != scope) {
        return Err("independent supporting input profile differs from campaign".into());
    }
    compare(recorded, expected)
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    #[test]
    fn verified_profile_not_receipt_selects_expected_identity() {
        let root = SupportingInputs {
            schema_version: 1,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "cargo-root-rust-discovery-v1".into(),
            digest: "a".repeat(64),
            files: 3,
        };
        let workspace = SupportingInputs {
            schema_version: 2,
            algorithm: root.algorithm.clone(),
            scope: "complete-snapshot-inputs-v2".into(),
            digest: "b".repeat(64),
            files: 4,
        };
        let v1 = serde_json::to_value(&root).unwrap();
        let v2 = serde_json::to_value(&workspace).unwrap();
        assert!(compare_profile(Some(&v1), Some(&root), None).is_ok());
        assert!(compare_profile(Some(&v2), Some(&workspace), Some("cargo-workspace-v2")).is_ok());
        assert!(compare_profile(Some(&v2), Some(&workspace), Some("cargo-workspace-v3")).is_ok());
        assert!(compare_profile(Some(&v2), Some(&workspace), Some("fest-confined-v1")).is_ok());
        assert!(compare_profile(Some(&v1), Some(&root), Some("fest-confined-v1")).is_err());
        assert!(compare_profile(Some(&v2), None, Some("fest-confined-v1")).is_err());
        assert!(compare_profile(Some(&v2), Some(&workspace), None).is_err());
        assert!(compare_profile(Some(&v1), Some(&root), Some("cargo-workspace-v2")).is_err());
        assert!(compare_profile(Some(&v1), Some(&root), Some("cargo-workspace-v3")).is_err());
        assert!(compare_profile(Some(&v2), None, Some("cargo-workspace-v2")).is_err());
        assert!(compare_profile(Some(&v2), Some(&workspace), Some("unknown")).is_err());
    }
}
