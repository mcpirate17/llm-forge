//! The identity a `forge mutation run` receipt carries in place of the Python
//! runner's file hashes.
//!
//! A native receipt names no Python runner file, because none ran. It names
//! the compiled runner instead: the executable's own digest, the source tree
//! it was built from, and a hash of each runner source as compiled in. A
//! receipt is current when those component hashes match this tree, or a
//! recorded lineage entry; a dirty build, whose binary no commit reproduces,
//! never validates.
//!
//! This branch is reached only by a receipt that declares `runner_kind`, and
//! only for generated campaigns: a reviewed receipt cannot opt out of its rule.

use serde_json::{Map, Value};

pub const RUNNER_KIND: &str = "forge-native";
pub const RUNNER_SCHEMA: &str = "llm.mutation-testing.native-runner.v1";

/// Repository-relative sources that decide a native verdict. The `forge` crate
/// compiles the same list in; a test there holds the two together.
pub const COMPONENT_PATHS: [&str; 15] = [
    "native/conductor-native/src/mutation_value.rs",
    "native/conductor-native/src/mutation_value_inputs.rs",
    "native/conductor-native/src/receipt_slim.rs",
    "native/forge/src/mutation_attribution.rs",
    "native/forge/src/mutation_bytecode.rs",
    "native/forge/src/mutation_campaign.rs",
    "native/forge/src/mutation_engine_cargo.rs",
    "native/forge/src/mutation_engine_fest.rs",
    "native/forge/src/mutation_engine_mull.rs",
    "native/forge/src/mutation_process.rs",
    "native/forge/src/mutation_run.rs",
    "native/forge/src/mutation_run_scope.rs",
    "native/forge/src/mutation_score.rs",
    "native/forge/src/mutation_scope.rs",
    "native/forge/src/mutation_snapshot.rs",
];

/// Whether this receipt claims the native runner produced it.
pub fn declared(payload: &Map<String, Value>) -> bool {
    payload.get("runner_kind").and_then(Value::as_str) == Some(RUNNER_KIND)
}

fn required_digest(block: &Map<String, Value>, key: &str) -> Result<(), String> {
    match block.get(key).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(()),
        _ => Err(format!("native_runner.{key} is missing")),
    }
}

/// The declared component map, once the identity block has the required shape.
pub fn components(payload: &Map<String, Value>) -> Result<&Map<String, Value>, String> {
    let block = payload
        .get("native_runner")
        .and_then(Value::as_object)
        .ok_or_else(|| "native_runner is missing".to_string())?;
    if block.get("schema_version").and_then(Value::as_str) != Some(RUNNER_SCHEMA) {
        return Err("native_runner schema is not current".into());
    }
    required_digest(block, "executable_sha256")?;
    required_digest(block, "compiled_source_sha256")?;
    required_digest(block, "compiled_git_commit")?;
    if block.get("source_matches_git") != Some(&Value::Bool(true)) {
        return Err("native_runner was built from a dirty source tree".into());
    }
    let map = block
        .get("components_sha256")
        .and_then(Value::as_object)
        .ok_or_else(|| "native_runner.components_sha256 is missing".to_string())?;
    let declared = map.keys().map(String::as_str).collect::<Vec<_>>();
    let mut expected = COMPONENT_PATHS.to_vec();
    let mut sorted = declared.clone();
    expected.sort_unstable();
    sorted.sort_unstable();
    if sorted != expected {
        return Err("native_runner.components_sha256 does not name the runner sources".into());
    }
    Ok(map)
}

/// Why a native receipt's runner cannot be accepted; empty when it can.
pub fn errors(
    payload: &Map<String, Value>,
    hash: impl Fn(&str) -> Option<String>,
    lineage: impl Fn(Option<&Value>) -> bool,
) -> Vec<String> {
    let map = match components(payload) {
        Ok(map) => map,
        Err(error) => return vec![error],
    };
    let current = map
        .iter()
        .all(|(path, recorded)| hash(path).as_deref() == recorded.as_str());
    if current || lineage(Some(&Value::Object(map.clone()))) {
        Vec::new()
    } else {
        vec!["native runner components match neither this tree nor any lineage entry".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receipt() -> Map<String, Value> {
        let map: Map<String, Value> = COMPONENT_PATHS
            .iter()
            .map(|path| ((*path).to_string(), json!(format!("hash-{path}"))))
            .collect();
        json!({
            "runner_kind": RUNNER_KIND,
            "native_runner": {
                "schema_version": RUNNER_SCHEMA,
                "executable_sha256": "exe",
                "compiled_source_sha256": "tree",
                "compiled_git_commit": "commit",
                "source_matches_git": true,
                "components_sha256": map,
            }
        })
        .as_object()
        .cloned()
        .unwrap()
    }

    fn current(path: &str) -> Option<String> {
        Some(format!("hash-{path}"))
    }

    #[test]
    fn the_python_audit_twin_names_the_same_sources() {
        let audit = include_str!("../../../src/conductor/mutation_patch_audit.py");
        let named = audit.matches("\"native/").count();
        assert_eq!(named, COMPONENT_PATHS.len());
        for path in COMPONENT_PATHS {
            assert!(audit.contains(&format!("\"{path}\"")), "{path}");
        }
    }

    #[test]
    fn a_receipt_is_native_only_when_it_says_so() {
        assert!(declared(&receipt()));
        assert!(!declared(&Map::new()));
        let other = json!({"runner_kind": "python"});
        assert!(!declared(other.as_object().unwrap()));
    }

    #[test]
    fn current_components_are_accepted_without_lineage() {
        assert!(errors(&receipt(), current, |_| false).is_empty());
    }

    #[test]
    fn stale_components_need_a_lineage_entry() {
        let stale = |path: &str| Some(format!("other-{path}"));
        assert_eq!(
            errors(&receipt(), stale, |_| false),
            vec!["native runner components match neither this tree nor any lineage entry"]
        );
        assert!(errors(&receipt(), stale, |_| true).is_empty());
    }

    #[test]
    fn a_tree_without_the_runner_sources_cannot_vouch_for_a_receipt() {
        assert_eq!(errors(&receipt(), |_| None, |_| false).len(), 1);
    }

    #[test]
    fn a_dirty_build_never_validates_even_through_lineage() {
        let mut payload = receipt();
        payload["native_runner"]["source_matches_git"] = json!(false);
        assert_eq!(
            errors(&payload, current, |_| true),
            vec!["native_runner was built from a dirty source tree"]
        );
    }

    #[test]
    fn a_partial_component_map_is_refused() {
        let mut payload = receipt();
        payload["native_runner"]["components_sha256"]
            .as_object_mut()
            .unwrap()
            .remove(COMPONENT_PATHS[0]);
        assert_eq!(
            errors(&payload, current, |_| true),
            vec!["native_runner.components_sha256 does not name the runner sources"]
        );
    }

    #[test]
    fn every_identity_field_is_required() {
        for key in [
            "schema_version",
            "executable_sha256",
            "compiled_source_sha256",
            "compiled_git_commit",
            "source_matches_git",
            "components_sha256",
        ] {
            let mut payload = receipt();
            payload["native_runner"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert_eq!(errors(&payload, current, |_| true).len(), 1, "{key}");
        }
    }
}
