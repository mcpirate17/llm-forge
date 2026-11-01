#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{commit, run};
use candidate_execution_fixture::{value, Case};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
fn manifest(version: &str) -> String {
    format!("[package]\nname='stage_fixture'\nversion='{version}'\n")
}
fn case() -> Case {
    let mut case = Case::new(Path::new(support::forge_binary::path()));
    case.staged_policy(|text| text.replace("timeout_seconds=2", "timeout_seconds=30")
        .replace("rust=['src/*.rs']", "rust=['src/*.rs','crate/src/*.rs']")
        .replace("memory_mb=128", "memory_mb=1024")
        .replace("command=['worker','echo','literal argument','{files}']",
            "command=['worker','candidate','crate-version','--base','{base}','--tree','{tree}','--execute','--','{files}']")
        .replace("version_command=['worker','echo','fixture-v1']",
            "version_command=['worker','candidate','crate-version','--tool-version']"));
    case.fixture.stage("crate/Cargo.toml", &manifest("1.0.0"));
    case.fixture
        .stage("crate/src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    commit(&case);
    case.delegated();
    case.grants["environment"] = json!({"FORGE_CRATE_INSTALLED_ROOTS":"[]"});
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    case
}
fn installed(case: &mut Case, version: &str) -> (PathBuf, Vec<u8>) {
    let site = case.fixture.root.join("operator-site");
    let directory = site.join(format!("stage_fixture-{version}.dist-info"));
    fs::create_dir_all(&directory).unwrap();
    for path in [&site, &directory] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.join("METADATA");
    let bytes =
        format!("Metadata-Version: 2.1\nName: stage-fixture\nVersion: {version}\n\n").into_bytes();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    case.grants["environment"]["FORGE_CRATE_INSTALLED_ROOTS"] =
        json!(serde_json::to_string(&[&site]).unwrap());
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(site));
    (path, bytes)
}
fn stage(version: &str, expected: i32, installed_version: Option<&str>) {
    let mut case = case();
    let metadata = installed_version.map(|version| installed(&mut case, version));
    case.fixture.stage("crate/Cargo.toml", &manifest(version));
    case.fixture
        .stage("crate/src/lib.rs", "pub fn value() -> u32 { 2 }\n");
    let before = case.source_state();
    let repo = case.fixture.root.join("repo");
    fs::write(repo.join("crate/Cargo.toml"), b"dirty original manifest\n").unwrap();
    fs::write(repo.join("crate/src/lib.rs"), b"dirty original source\n").unwrap();
    let mut command = case.command();
    command.arg("--execute");
    let report = value(run(command), expected);
    assert_eq!(report["files"], json!(["crate/src/lib.rs"]));
    assert_eq!(
        report["result"]["status"],
        if expected == 0 { "passed" } else { "failed" }
    );
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    let inner: Value =
        serde_json::from_str(stages[1]["report"]["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(inner["check"], "crate-version");
    assert_eq!(inner["executed"], true);
    if let Some((path, bytes)) = &metadata {
        assert_eq!(
            inner["plan"]["installed_metadata"],
            json!([{"path":path,
            "sha256":format!("{:x}", Sha256::digest(bytes))}])
        );
        assert_eq!(fs::read(path).unwrap(), *bytes);
    } else {
        assert_eq!(inner["plan"]["installed_metadata"], json!([]));
    }
    assert_eq!(inner["result"]["files"], json!(["crate/Cargo.toml"]));
    let findings = inner["result"]["findings"].as_array().unwrap();
    assert_eq!(findings.len(), usize::from(expected != 0));
    if expected != 0 {
        let (rule, severity) = if metadata.is_some() {
            ("installed-version-drift", "high")
        } else {
            ("source-changed-without-version-bump", "critical")
        };
        assert_eq!(findings[0]["rule_id"], rule);
        assert_eq!(findings[0]["severity"], severity);
    }
    assert_eq!(
        fs::read(repo.join("crate/Cargo.toml")).unwrap(),
        b"dirty original manifest\n"
    );
    assert_eq!(
        fs::read(repo.join("crate/src/lib.rs")).unwrap(),
        b"dirty original source\n"
    );
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn native_crate_version_stage_passes_captured_bump_with_explicit_empty_installed_roots() {
    stage("1.0.1", 0, None);
}
#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn native_crate_version_stage_rejects_unchanged_version_and_cleans_owned_processes() {
    stage("1.0.0", 1, None);
}
#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn native_crate_version_stage_reads_explicitly_granted_metadata_and_reports_drift() {
    stage("1.0.1", 1, Some("1.0.0"));
}
