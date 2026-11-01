#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use serde_json::json;
use std::{fs, path::PathBuf, time::Duration};

fn case(mode: &str) -> Case {
    let mut case = Case::new(std::path::Path::new(support::forge_binary::path()));
    case.staged_policy(|text| text
        .replace("timeout_seconds=2", "timeout_seconds=30")
        .replace("memory_mb=128", "memory_mb=2048")
        .replace("command=['worker','echo','literal argument','{files}']", &format!("command=['worker','candidate','cargo-lint','--mode','{mode}','--execute','--','{{files}}']"))
        .replace("version_command=['worker','echo','fixture-v1']", &format!("version_command=['worker','candidate','cargo-lint','--mode','{mode}','--tool-version']")));
    case.fixture.stage(
        "forge.toml",
        "schema_version=1\n[conductor]\ncrate_roster='lint-roster.toml'\n",
    );
    case.fixture.stage(
        "lint-roster.toml",
        "[crates]\ntested=['.']\nlinted=['.']\n[globs]\nmanifests=['Cargo.toml']\n",
    );
    case.fixture.stage(
        "Cargo.toml",
        "[package]\nname='native_lint_fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    case.fixture.stage(
        "Cargo.lock",
        "version=4\n[[package]]\nname='native_lint_fixture'\nversion='0.1.0'\n",
    );
    case.fixture.stage("src/lib.rs", "pub fn clean() {}\n");
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":1024,"processes":64});
    case.grants["max_wall_seconds"] = json!(45);
    case.grants["max_cpu_seconds"] = json!(45);
    case.grants["max_address_space_mib"] = json!(4096);
    case
}
fn provision(case: &mut Case) {
    case.delegated();
    let bin =
        PathBuf::from(std::env::var_os("FORGE_TEST_CARGO_BIN").expect("explicit native toolchain"));
    case.grants["environment"] = json!({"FORGE_CARGO_BIN":bin,"FORGE_CARGO_EXTRA_PATH":"/usr/bin"});
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(bin.parent().unwrap().canonicalize().unwrap()));
}
fn run(case: &Case, execute: bool) -> std::process::Output {
    let mut command = case.command();
    if execute {
        command.arg("--execute");
    }
    forge::process_probe::run(&mut command, Duration::from_secs(50), 1024 * 1024).unwrap()
}

#[test]
fn granted_native_lint_dry_run_prepares_two_owned_external_stages() {
    let case = case("fmt");
    let before = case.source_state();
    let report = value(run(&case, false), 0);
    assert_eq!(report["executed"], false);
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(
        stages[0]["command"],
        json!([
            "worker",
            "candidate",
            "cargo-lint",
            "--mode",
            "fmt",
            "--tool-version"
        ])
    );
    assert!(stages[1]["command"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "src/lib.rs"));
    assert!(stages.iter().all(|stage| stage["report"].is_null()));
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN and delegated FORGE_TEST_CGROUP_PARENT"]
fn delegated_native_fmt_uses_staged_source_and_preserves_dirty_original() {
    let mut case = case("fmt");
    provision(&mut case);
    case.fixture.stage(
        "src/lib.rs",
        "pub fn clean( ){let value=  1;println!(\"{}\",value);}\n",
    );
    let original = "pub fn clean() {}\n";
    fs::write(case.fixture.root.join("repo/src/lib.rs"), original).unwrap();
    let before = case.source_state();
    let report = value(run(&case, true), 1);
    assert_eq!(report["result"]["status"], "failed");
    assert_eq!(report["stages"].as_array().unwrap().len(), 2);
    assert!(report["result"]["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("Diff in"));
    assert_eq!(
        fs::read_to_string(case.fixture.root.join("repo/src/lib.rs")).unwrap(),
        original
    );
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN and delegated FORGE_TEST_CGROUP_PARENT"]
fn delegated_native_clippy_ignores_candidate_compiler_redirect_and_cleans_every_stage() {
    let mut case = case("clippy");
    provision(&mut case);
    case.fixture.stage(
        ".cargo/config.toml",
        "[build]\nrustc='/must-not-run'\nrustc-wrapper='/must-not-run'\n",
    );
    let before = case.source_state();
    let report = value(run(&case, true), 0);
    assert_eq!(report["result"]["status"], "passed");
    assert_eq!(report["stages"].as_array().unwrap().len(), 2);
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .starts_with("clippy "));
    case.assert_preserved(&before);
    assert!(!case.fixture.root.join("repo/target").exists());
    case.fixture
        .stage("src/lib.rs", "pub fn clean() { let unused = 1; }\n");
    let before = case.source_state();
    let report = value(run(&case, true), 1);
    assert_eq!(report["result"]["status"], "failed");
    assert!(report["result"]["stderr_tail"]
        .as_str()
        .unwrap()
        .contains("unused variable"));
    case.assert_preserved(&before);
}
