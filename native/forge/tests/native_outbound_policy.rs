#![cfg(target_os = "linux")]
//! Admission exercises the shipped policy and held bindings without a child launch.
use forge::harness::policy::Policy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    state: PathBuf,
    binary: PathBuf,
    document: Value,
}
fn sha(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
fn mode(path: &Path, bits: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        mode(&root, 0o700);
        let source = root.join("source");
        let state = root.join("state");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&state).unwrap();
        mode(&source, 0o700);
        mode(&state, 0o700);
        git2::Repository::init(&source).unwrap();
        mode(&source.join(".git"), 0o700);
        let data = source.join("data.txt");
        fs::write(&data, b"bounded input").unwrap();
        mode(&data, 0o600);
        let binary = root.join("worker");
        fs::copy("/usr/bin/true", &binary).unwrap();
        mode(&binary, 0o700);
        let schema =
            r#"{"type":"object","properties":{},"required":[],"additionalProperties":false}"#;
        let document = json!({
            "version":1, "source_root":source, "write_paths":[], "read_only":[binary], "programs":{},
            "aggregate":{"cpu_millis":1000,"memory_mib":1024,"processes":64},
            "session":{"cpu_millis":500,"memory_mib":64,"processes":8},
            "max_sessions":1,"max_retained_sessions":1,"max_tasks":1,"lifetime_seconds":30,
            "session_seconds":10,"retention_seconds":5,"replay_bytes":4096,"log_bytes":4096,
            "outbound_mcp":{
                "public_name":"outbound_read", "remote_name":"read_file",
                "executable":{"path":binary,"sha256":sha(&binary)}, "fixed_arguments":[],
                "pinned_inputs":[{"path":data,"sha256":sha(&data)}], "read_only":[binary,data],
                "input_schema_json":schema, "resources":{"cpu_millis":500,"memory_mib":64,"processes":8},
                "max_wall_seconds":10
            }
        });
        Self {
            _temp: temp,
            root,
            source,
            state,
            binary,
            document,
        }
    }
    fn validate(&self, document: Value) -> anyhow::Result<Policy> {
        serde_json::from_value::<Policy>(document)?.validate(&self.state)
    }
    fn rejects(&self, document: Value, expected: &str) {
        let error = match self.validate(document) {
            Ok(_) => panic!("expected rejection: {expected}"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(
            message.contains(expected),
            "expected {expected:?}, got {message:?}"
        );
    }
}

#[test]
fn valid_pinned_executable_and_source_input_are_admitted() {
    let fixture = Fixture::new();
    let policy = fixture.validate(fixture.document.clone()).unwrap();
    assert!(policy.outbound_mcp.is_some());
    assert!(fixture.binary.is_file());
}

#[test]
fn read_grants_reject_ungranted_state_git_and_credential_paths() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    fs::write(&outside, b"outside").unwrap();
    mode(&outside, 0o600);
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["read_only"][1] = json!(outside);
    fixture.rejects(policy, "outbound read exceeds operator grants");
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["read_only"][1] = json!(fixture.source.join(".git"));
    fixture.rejects(policy, "execution grant paths overlap");
    let mut policy = fixture.document.clone();
    policy["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(fixture.state));
    policy["outbound_mcp"]["read_only"][1] = json!(fixture.state);
    fixture.rejects(policy, "state must be outside");
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME required by admission"));
    let credential = [".ssh", ".aws", ".gnupg", ".config", ".cargo"]
        .iter()
        .map(|name| home.join(name))
        .find(|path| path.exists())
        .unwrap_or(home)
        .canonicalize()
        .unwrap();
    let mut policy = fixture.document.clone();
    policy["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(credential));
    policy["outbound_mcp"]["read_only"][1] = json!(credential);
    let error = match fixture.validate(policy) {
        Ok(_) => panic!("credential grant accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("execution grant paths overlap")
            || error.contains("outbound read exposes HOME"),
        "{error}"
    );
}

#[test]
fn symlink_drift_pins_schemas_and_budget_fail_closed() {
    let fixture = Fixture::new();
    let link = fixture.source.join("link");
    symlink(fixture.source.join("data.txt"), &link).unwrap();
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["read_only"][1] = json!(link);
    fixture.rejects(policy, "contains a symlink");
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["executable"]["sha256"] = json!("0".repeat(64));
    fixture.rejects(policy, "execution program digest mismatch");
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["input_schema_json"] = json!(r#"{"type":"object","type":"object"}"#);
    fixture.rejects(policy, "duplicate");
    let mut policy = fixture.document.clone();
    policy["outbound_mcp"]["public_name"] = json!("terminal_spawn");
    fixture.rejects(policy, "invalid outbound public name");
    let mut policy = fixture.document.clone();
    policy["aggregate"]["memory_mib"] = json!(128);
    fixture.rejects(policy, "aggregate budget");
}
