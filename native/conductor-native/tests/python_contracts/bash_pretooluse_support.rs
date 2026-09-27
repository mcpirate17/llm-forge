//! Rust fixture builder for the frozen Bash PreToolUse parity corpus.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MANAGED_ENV: &[&str] = &[
    "CRG_GATE_REPO_ROOT",
    "PROJECT_DIR",
    "CRG_GATE_STATE_DIR",
    "CRG_DATA_DIR",
    "GOVERNANCE_OWNER",
    "LOCAL_AI_RUNTIME",
];

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forge/tests/fixtures")
        .join(name)
}

pub fn load(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

pub fn make_git_repo(parent: &Path, label: &str) -> PathBuf {
    let root = parent.join(format!("{label}-repo"));
    fs::create_dir(&root).unwrap();
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    root
}

pub fn common_dir(root: &Path) -> PathBuf {
    root.join(".git").canonicalize().unwrap()
}

pub fn mark_graph_used(state_dir: &Path, session_id: &str) {
    let key = format!("{:x}", Sha256::digest(session_id.as_bytes()));
    fs::create_dir_all(state_dir).unwrap();
    fs::write(state_dir.join(format!("{key}.graph-used")), b"1").unwrap();
}

pub fn claim_id(owner: &str, paths: &[Value], created_at: &str, expires_at: &str) -> String {
    let mut fields = BTreeMap::<&str, Value>::new();
    fields.insert("created_at", json!(created_at));
    fields.insert("expires_at", json!(expires_at));
    fields.insert("justification", json!("because"));
    fields.insert("owner", json!(owner));
    fields.insert("paths", Value::Array(paths.to_vec()));
    let canonical = serde_json::to_string(&fields).unwrap();
    let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    format!("claim-{}", &digest[..20])
}

pub fn write_claims(root: &Path, claims: &[Value]) {
    let mut rows = Vec::new();
    for claim in claims {
        let owner = claim["owner"].as_str().unwrap();
        let paths = claim["paths"].as_array().unwrap();
        let created = claim["created_at"].as_str().unwrap();
        let expires = claim["expires_at"].as_str().unwrap();
        rows.push(json!({
            "claim_id": claim_id(owner, paths, created, expires),
            "owner": owner, "paths": paths, "justification": "because",
            "created_at": created, "expires_at": expires,
        }));
    }
    let gov = common_dir(root).join("governance");
    fs::create_dir_all(&gov).unwrap();
    fs::write(
        gov.join("ownership-claims.json"),
        json!({"schema_version": 1, "claims": rows}).to_string(),
    )
    .unwrap();
}

pub fn detach_head(root: &Path) {
    fs::write(
        root.join(".git/HEAD"),
        "0000000000000000000000000000000000000000\n",
    )
    .unwrap();
}
