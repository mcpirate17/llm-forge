//! Observed legacy runner inputs for read-only maintenance reports.
use crate::{
    candidate_mutation_runner::{BINDINGS, COMPONENTS},
    mutation_live_inputs::LiveInputs,
};
use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub(crate) struct Legacy {
    held: Option<LiveInputs>,
    pub runtime: Value,
    pub diagnostics: Vec<String>,
}
impl Legacy {
    pub(crate) fn capture(root: Option<&Path>) -> Result<Self> {
        let Some(root) = root else {
            return Ok(Self {
                held: None,
                runtime: unavailable("legacy component root was not selected"),
                diagnostics: Vec::new(),
            });
        };
        let mut held = LiveInputs::open(root)?;
        let mut components = BTreeMap::new();
        let mut error = None;
        for path in COMPONENTS {
            match held.read(path)? {
                Some(bytes) => {
                    components.insert(path, hex(bytes));
                }
                None => {
                    error.get_or_insert_with(|| {
                        format!("mutation runner component is missing: {path}")
                    });
                }
            }
        }
        let mut bindings = BTreeMap::new();
        for path in BINDINGS {
            if let Some(bytes) = held.read(path)? {
                bindings.insert(path, hex(bytes));
            }
        }
        let mut diagnostics = Vec::new();
        let lineage = match held.read("conductor/mutation_runner_lineage.json")? {
            None => None,
            Some(bytes) => {
                let parsed = std::str::from_utf8(bytes)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| {
                        crate::tool_schema::parse_document_with_limit(text, 2 * 1024 * 1024)
                    });
                match parsed {
                    Ok(value)
                        if value["schema_version"].as_i64() == Some(1)
                            && value["entries"].is_array() =>
                    {
                        Some(value)
                    }
                    Ok(_) => {
                        diagnostics.push(
                            "legacy runner lineage unavailable: invalid schema or entries".into(),
                        );
                        None
                    }
                    Err(error) => {
                        diagnostics.push(format!("legacy runner lineage unavailable: {error:#}"));
                        None
                    }
                }
            }
        };
        let runtime = if let Some(error) = error {
            json!({"runner":{"components":null,"error":error,"mutation_testing_sha256":null},"binding_sha256":bindings,"lineage":null})
        } else {
            json!({"runner":{"mutation_testing_sha256":components["conductor/mutation_testing.py"],"components":components,"error":null},"binding_sha256":bindings,"lineage":lineage})
        };
        held.verify()?;
        Ok(Self {
            held: Some(held),
            runtime,
            diagnostics,
        })
    }
    pub(crate) fn verify(&self) -> Result<()> {
        if let Some(held) = &self.held {
            held.verify()?;
        }
        Ok(())
    }
}
fn unavailable(error: &str) -> Value {
    json!({"runner":{"components":null,"error":error,"mutation_testing_sha256":null},"binding_sha256":{},"lineage":null})
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unspecified_identity_is_diagnostic() {
        let legacy = Legacy::capture(None).unwrap();
        assert!(legacy.runtime["runner"]["components"].is_null());
        assert!(legacy.runtime["runner"]["error"]
            .as_str()
            .unwrap()
            .contains("not selected"));
        legacy.verify().unwrap();
    }
    #[test]
    fn missing_component_is_held_as_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let legacy = Legacy::capture(Some(root.path())).unwrap();
        assert!(legacy.runtime["runner"]["components"].is_null());
        assert!(legacy.runtime["runner"]["error"]
            .as_str()
            .unwrap()
            .contains("missing"));
        legacy.verify().unwrap();
        std::fs::create_dir_all(root.path().join("conductor")).unwrap();
        std::fs::write(root.path().join(COMPONENTS[0]), b"changed").unwrap();
        assert!(legacy.verify().is_err());
    }
    #[test]
    fn observed_group_writable_components_and_lineage_detect_drift() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        for path in COMPONENTS.into_iter().chain(BINDINGS) {
            let file = root.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::set_permissions(
                file.parent().unwrap(),
                std::fs::Permissions::from_mode(0o775),
            )
            .unwrap();
            std::fs::write(&file, path.as_bytes()).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o664)).unwrap();
        }
        let lineage = root.path().join("conductor/mutation_runner_lineage.json");
        std::fs::write(&lineage, br#"{"schema_version":1,"entries":[]}"#).unwrap();
        let held = Legacy::capture(Some(root.path())).unwrap();
        assert!(held.runtime["runner"]["components"].is_object());
        assert!(held.runtime["lineage"].is_object());
        held.verify().unwrap();
        std::fs::write(lineage, b"{}").unwrap();
        assert!(held.verify().is_err());
    }
    #[test]
    fn malformed_lineage_is_diagnostic_and_remains_held() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("conductor")).unwrap();
        let path = root.path().join("conductor/mutation_runner_lineage.json");
        std::fs::write(&path, b"{").unwrap();
        let held = Legacy::capture(Some(root.path())).unwrap();
        assert!(held.runtime["lineage"].is_null());
        assert_eq!(held.diagnostics.len(), 1);
        held.verify().unwrap();
        std::fs::write(path, b"{}").unwrap();
        assert!(held.verify().is_err());
    }
}
