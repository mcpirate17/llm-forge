//! Native receipt source freshness. These self-reported identities do not attest
//! the producer, compiler, build environment, or executable authenticity.
use serde_json::{Map, Value};

pub const RUNNER_KIND: &str = "forge-native";
pub const RUNNER_SCHEMA: &str = "llm.mutation-testing.native-runner.v2";
/// Reserved key for a fingerprint obtained from a complete trusted inventory.
pub const FINGERPRINT_KEY: &str = "@forge-native-source-fingerprint-v1";

pub fn declared(payload: &Map<String, Value>) -> bool {
    payload.get("runner_kind").and_then(Value::as_str) == Some(RUNNER_KIND)
}

fn hex(value: Option<&str>, length: usize) -> bool {
    value.is_some_and(|s| {
        s.len() == length
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Validate the self-reported identity and compare it with independently held
/// current inputs. Legacy partial component maps cannot establish freshness.
pub fn errors(
    payload: &Map<String, Value>,
    hash: impl Fn(&str) -> Option<String>,
    _lineage: impl Fn(Option<&Value>) -> bool,
) -> Vec<String> {
    let Some(block) = payload.get("native_runner").and_then(Value::as_object) else {
        return vec!["native_runner is missing".into()];
    };
    if block.get("schema_version").and_then(Value::as_str) != Some(RUNNER_SCHEMA) {
        return vec!["native_runner schema is not current".into()];
    }
    const KEYS: [&str; 6] = [
        "schema_version",
        "forge_version",
        "executable_sha256",
        "compiled_source_sha256",
        "compiled_git_commit",
        "source_matches_git",
    ];
    if block.len() != KEYS.len() || KEYS.iter().any(|key| !block.contains_key(*key)) {
        return vec!["native_runner identity fields do not match schema".into()];
    }
    if block
        .get("forge_version")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return vec!["native_runner.forge_version is missing".into()];
    }
    for key in ["executable_sha256", "compiled_source_sha256"] {
        if !hex(block.get(key).and_then(Value::as_str), 64) {
            return vec![format!("native_runner.{key} is malformed or missing")];
        }
    }
    if !hex(block.get("compiled_git_commit").and_then(Value::as_str), 40) {
        return vec!["native_runner.compiled_git_commit is malformed or missing".into()];
    }
    if block.get("source_matches_git") != Some(&Value::Bool(true)) {
        return vec!["native_runner was built from a dirty source tree".into()];
    }
    let expected = block.get("compiled_source_sha256").and_then(Value::as_str);
    if hash(FINGERPRINT_KEY).as_deref() != expected {
        return vec![
            "native runner source fingerprint is unavailable or differs from this tree".into(),
        ];
    }
    let expanded = match crate::receipt_slim::expand_receipt(&Value::Object(payload.clone())) {
        Ok(value) => value,
        Err(error) => return vec![format!("cannot expand native receipt: {error}")],
    };
    let Some(expanded) = expanded.as_object() else {
        return vec!["native receipt is not an object".into()];
    };
    crate::mutation_source_coverage::validate(expanded)
        .err()
        .into_iter()
        .collect()
}

#[cfg(feature = "python")]
mod python {
    use pyo3::prelude::*;
    use std::path::Path;

    #[pyfunction]
    fn native_source_fingerprint(root: &str) -> PyResult<String> {
        let root = Path::new(root);
        let files = crate::source_fingerprint::files(root)
            .map_err(|error| pyo3::exceptions::PyOSError::new_err(error.to_string()))?;
        crate::source_fingerprint::fingerprint(root, &files)
            .map_err(|error| pyo3::exceptions::PyOSError::new_err(error.to_string()))
    }

    #[pyfunction]
    fn native_runner_rejection(
        receipt_json: &str,
        fingerprint: Option<String>,
    ) -> PyResult<Option<String>> {
        let payload: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(receipt_json)
                .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
        Ok(super::errors(&payload, |_| fingerprint.clone(), |_| false)
            .into_iter()
            .next())
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(native_source_fingerprint, module)?)?;
        module.add_function(wrap_pyfunction!(native_runner_rejection, module)?)
    }
}

#[cfg(feature = "python")]
pub use python::register;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receipt() -> Map<String, Value> {
        json!({"runner_kind": RUNNER_KIND,
        "source_sha256": {"src/a.rs": "pin"},
        "mutants": [{"path":"src/a.rs", "outcome":"KILLED"}],
        "source_coverage": {"schema_version":1,"complete":true,"scored_by_source":{"src/a.rs":1},"missing":[]},
        "native_runner": {
            "schema_version": RUNNER_SCHEMA,
            "forge_version": "0.0.0",
            "executable_sha256": "a".repeat(64),
            "compiled_source_sha256": "b".repeat(64),
            "compiled_git_commit": "c".repeat(40),
            "source_matches_git": true
        }})
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn fingerprint_must_be_current_and_cannot_be_waived_by_lineage() {
        assert!(errors(&receipt(), |_| Some("b".repeat(64)), |_| false).is_empty());
        assert!(!errors(&receipt(), |_| Some("c".repeat(64)), |_| true).is_empty());
        assert!(!errors(&receipt(), |_| None, |_| true).is_empty());
    }

    #[test]
    fn legacy_or_malformed_identity_is_rejected() {
        let mut value = receipt();
        value["native_runner"]["schema_version"] = json!("llm.mutation-testing.native-runner.v1");
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| true).is_empty());
        value["native_runner"]["schema_version"] = json!(RUNNER_SCHEMA);
        value["native_runner"]["executable_sha256"] = json!("not-a-digest");
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| true).is_empty());
        let mut value = receipt();
        value["native_runner"]["unexpected"] = json!(true);
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| true).is_empty());
    }

    #[test]
    fn native_identity_rejects_absent_or_forged_source_coverage() {
        let mut value = receipt();
        value.remove("source_coverage");
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| false).is_empty());
        value = receipt();
        value["source_coverage"]["scored_by_source"]["src/a.rs"] = json!(2);
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| false).is_empty());
        value = receipt();
        value["source_sha256"]["src/b.rs"] = json!("pin");
        assert!(!errors(&value, |_| Some("b".repeat(64)), |_| false).is_empty());
    }

    #[test]
    fn full_inventory_detects_dependencies_and_new_sources() {
        use crate::source_fingerprint::{self, EXTRA_FILES, SOURCE_TREES};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "forge-native-fingerprint-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let root = directory.as_path();
        let write = |path: &str, contents: &str| {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        };
        for name in SOURCE_TREES {
            for suffix in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
                write(&format!("native/{name}/{suffix}"), "original");
            }
        }
        for path in EXTRA_FILES {
            write(path, "original");
        }
        write("native/forge/src/process_group.rs", "original");
        let current = || {
            let files = source_fingerprint::files(root).unwrap();
            source_fingerprint::fingerprint(root, &files).unwrap()
        };
        let first = current();
        write("native/forge/src/process_group.rs", "changed");
        assert_ne!(first, current());
        write("native/forge/src/process_group.rs", "original");
        assert_eq!(first, current());
        for path in [
            "native/forge/Cargo.lock",
            "native/forge-cli/build.rs",
            EXTRA_FILES[3],
            "src/conductor/memory_sources.toml",
            "src/conductor/embedding_routes.toml",
        ] {
            write(path, "changed");
            assert_ne!(first, current(), "{path}");
            write(path, "original");
        }
        write("native/forge/src/new_module.rs", "new");
        assert_ne!(first, current());
        std::fs::remove_dir_all(root).unwrap();
    }
}
