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
use serde_json::json;
use std::{fs, path::Path};

fn case() -> Case {
    let mut case = Case::new(Path::new(support::forge_binary::path()));
    case.staged_policy(|text| text.replace("timeout_seconds=2", "timeout_seconds=30")
        .replace("memory_mb=128", "memory_mb=1024")
        .replace("command=['worker','echo','literal argument','{files}']",
            "command=['worker','candidate','scan','--check','rust-style-scan','--base','{base}','--tree','{tree}','--execute','--','{files}']")
        .replace("version_command=['worker','echo','fixture-v1']",
            "version_command=['worker','candidate','scan','--check','rust-style-scan','--tool-version']"));
    case.fixture.stage(
        "src/security/run.rs",
        "fn run() -> u32 {\n    let _old = Some(1).unwrap();\n    2\n}\n",
    );
    commit(&case);
    case.delegated();
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    case
}

fn stage(candidate: &str, expected: i32) {
    let case = case();
    case.fixture.stage("src/security/run.rs", candidate);
    let before = case.source_state();
    let original = case.fixture.root.join("repo/src/security/run.rs");
    fs::write(&original, b"dirty original must survive\n").unwrap();
    let output = run({
        let mut cmd = case.command();
        cmd.arg("--execute");
        cmd
    });
    let report = value(output, expected);
    assert_eq!(report["files"], json!(["src/security/run.rs"]));
    assert_eq!(
        report["result"]["status"],
        if expected == 0 { "passed" } else { "failed" }
    );
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .contains("forge-native-candidate-scan"));
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    let command = &stages[1]["report"];
    assert_eq!(command["exit_code"], expected);
    let scan: serde_json::Value =
        serde_json::from_str(command["stdout"].as_str().unwrap()).unwrap();
    let findings = scan["findings"].as_array().unwrap();
    assert_eq!(findings.len(), usize::from(expected != 0));
    if expected != 0 {
        assert_eq!(findings[0]["rule"], "failure/rust-unwrap");
        assert_eq!(findings[0]["line"], 3);
    }
    assert_eq!(
        fs::read(original).unwrap(),
        b"dirty original must survive\n"
    );
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn native_scan_stage_ignores_unchanged_finding() {
    stage(
        "fn run() -> u32 {\n    let _old = Some(1).unwrap();\n    3\n}\n",
        0,
    );
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn native_scan_stage_reports_finding_on_changed_line() {
    stage(
        "fn run() -> u32 {\n    let _old = Some(1).unwrap();\n    Some(3).unwrap()\n}\n",
        1,
    );
}
