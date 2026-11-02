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
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const POLICY: &str = "policies/main.toml";
fn case_with_delegation(check: &str, base: &[(&str, &str)], delegated: bool) -> Case {
    let program = Path::new(support::forge_binary::path());
    let mut magic = [0; 4];
    fs::File::open(program)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(
        &magic, b"\x7fELF",
        "FORGE_TEST_BIN must be the installed native CLI"
    );
    let mut case = Case::new(program);
    case.grants["programs"] = json!({"forge":program.canonicalize().unwrap()});
    let classes = if check == "config-parse" {
        "['config','notebook']"
    } else {
        "['python']"
    };
    case.staged_policy(|text|text.replace("[checks.zeta]",&format!("[checks.{check}]"))
        .replace("timeout_seconds=2","timeout_seconds=30")
        .replace("memory_mb=128","memory_mb=1024\nseverity='low'\nmax_output_chars=65536")
        .replace("[classes]\n","[classes]\npython=['*.py','**/*.py']\n")
        .replace("\nclasses=['rust']\ncommand=",&format!("\nclasses={classes}\ncommand="))
        .replace("command=['worker','echo','literal argument','{files}']",&format!("command=['forge','candidate','analyze','--check','{check}','--repo','{{snapshot}}','--base','{{base}}','--tree','{{tree}}','--policy','{POLICY}','--today','2030-01-10','--execute','--','{{files}}']"))
        .replace("version_command=['worker','echo','fixture-v1']",&format!("version_command=['forge','candidate','analyze','--check','{check}','--tool-version']")));
    for (path, text) in base {
        case.fixture.stage(path, text);
    }
    commit(&case);
    if delegated {
        case.delegated();
    }
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    assert!(case.grants["python"].is_null());
    case
}

fn case(check: &str, base: &[(&str, &str)]) -> Case {
    case_with_delegation(check, base, true)
}

fn stage(case: &Case, check: &str, child_code: i32) -> Value {
    let before = case.source_state();
    let mut command = case.command_for("execute", "index", &[check]);
    command.arg("--execute");
    let output = run(command);
    assert_eq!(
        output.status.code(),
        Some(i32::from(child_code != 0)),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = value(output, i32::from(child_code != 0));
    assert_eq!(
        report["result"]["status"],
        if child_code == 0 { "passed" } else { "failed" }
    );
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .starts_with("forge-native-analysis 1 "));
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    let parent = PathBuf::from(case.grants["cgroup_parent"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    for stage in stages {
        let observed = &stage["report"];
        assert_eq!(observed["cleanup_complete"], true);
        assert_eq!(observed["resources"]["verified"], true);
        assert_eq!(observed["errors"], json!([]));
        let group = Path::new(observed["resources"]["path"].as_str().unwrap());
        assert_eq!(group.parent(), Some(parent.as_path()));
        assert!(
            !group.exists(),
            "owned stage cgroup remains: {}",
            group.display()
        );
        assert!(!Path::new(stage["snapshot"].as_str().unwrap()).exists());
    }
    assert_eq!(stages[0]["report"]["exit_code"], 0);
    assert_eq!(stages[1]["report"]["exit_code"], child_code);
    case.assert_preserved(&before);
    report
}
fn native(report: &Value, check: &str) -> Value {
    let inner: Value =
        serde_json::from_str(report["stages"][1]["report"]["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(inner["schema_version"], 1);
    assert_eq!(inner["check"], check);
    assert_eq!(inner["executed"], true);
    let findings = inner["result"]["findings"].as_array().unwrap();
    let mapped = report["result"]["findings"].as_array().unwrap();
    assert_eq!(
        mapped.len(),
        findings.len(),
        "structured findings must not become one generic command observation"
    );
    for (original, mapped) in findings.iter().zip(mapped) {
        for field in ["check_id", "rule_id", "severity", "path", "line"] {
            assert_eq!(
                mapped[field], original[field],
                "native finding field changed: {field}"
            );
        }
    }
    inner
}
fn has(inner: &Value, rule: &str) -> bool {
    inner["result"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["rule_id"] == rule)
}
fn incomplete(report: &Value, reason: &str) {
    let finding = &report["result"]["findings"][0];
    assert_eq!(finding["rule_id"], "analyzer-crash");
    assert_eq!(
        finding["severity"], "critical",
        "incomplete coverage must override low command severity"
    );
    assert!(report["stages"][1]["report"]["stderr"]
        .as_str()
        .unwrap()
        .contains("analysis incomplete"));
    let stderr = report["stages"][1]["report"]["stderr"].as_str().unwrap();
    assert!(
        stderr.contains(reason),
        "wrong failure: {stderr}; expected {reason}"
    );
}

fn provider(case: &mut Case, grant_root: bool) -> (PathBuf, Vec<u8>) {
    let operator = case.fixture.root.join("operator-providers");
    fs::create_dir(&operator).unwrap();
    fs::set_permissions(&operator, fs::Permissions::from_mode(0o700)).unwrap();
    let path = operator.join("observation.json");
    let bytes = serde_json::to_vec(&json!({"schema_version":1,"implementation":"cpython",
        "version":"3.12","platform":"linux","stdlib":["os","sys","typing"],
        "modules":[{"name":"requests","distributions":["requests"]}]}))
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    case.grants["environment"] = json!({"FORGE_IMPORT_PROVIDERS":json!({"path":path,
        "sha256":format!("{:x}",Sha256::digest(&bytes))}).to_string()});
    if grant_root {
        case.grants["read_only"]
            .as_array_mut()
            .unwrap()
            .push(json!(&operator));
    }
    assert!(!path.starts_with(case.fixture.root.join("repo")));
    assert!(!path.starts_with(case.fixture.root.join("scratch")));
    (path, bytes)
}
fn dirty(case: &Case, path: &str) {
    fs::write(
        case.fixture.root.join("repo").join(path),
        b"dirty original must survive\n",
    )
    .unwrap();
}
fn assert_dirty(case: &Case, path: &str) {
    assert_eq!(
        fs::read(case.fixture.root.join("repo").join(path)).unwrap(),
        b"dirty original must survive\n"
    );
}
fn function(name: &str) -> String {
    format!("def {name}():\n    a = 1\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    g = 7\n    h = 8\n    return a\n")
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_config_analysis_stage_preserves_critical_parse_findings_and_owned_cleanup() {
    for (candidate, code) in [("value = 2\n", 0), ("value = [\n", 1)] {
        let case = case("config-parse", &[("settings.toml", "value = 1\n")]);
        case.fixture.stage("settings.toml", candidate);
        dirty(&case, "settings.toml");
        let inner = native(&stage(&case, "config-parse", code), "config-parse");
        assert_eq!(has(&inner, "malformed-config"), code != 0);
        if code != 0 {
            assert_eq!(inner["result"]["findings"][0]["severity"], "critical");
        }
        assert_dirty(&case, "settings.toml");
    }
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_python_analysis_stage_keeps_critical_rule_severity_and_line_under_low_policy_severity() {
    for (candidate, code) in [("value = 2\n", 0), ("value = eval('1')\n", 1)] {
        let case = case("python-ast", &[("a.py", "value = 1\n")]);
        case.fixture.stage("a.py", candidate);
        dirty(&case, "a.py");
        let inner = native(&stage(&case, "python-ast", code), "python-ast");
        assert_eq!(has(&inner, "dynamic-execution"), code != 0);
        if code != 0 {
            assert_eq!(inner["result"]["findings"][0]["line"], 1);
        }
        assert_dirty(&case, "a.py");
    }
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_duplicate_analysis_stage_compares_unchanged_peers_and_retains_attributed_findings() {
    let base = function("candidate");
    let peer = function("peer");
    let before = format!("{base}\nmarker = 1\n");
    for (candidate, code) in [
        (format!("{base}\nmarker = 2\n"), 0),
        (before.replace("def candidate()", "def renamed()"), 1),
    ] {
        let case = case(
            "duplicate-function-bodies",
            &[("a.py", &before), ("peer.py", &peer)],
        );
        case.fixture.stage("a.py", &candidate);
        dirty(&case, "a.py");
        let inner = native(
            &stage(&case, "duplicate-function-bodies", code),
            "duplicate-function-bodies",
        );
        assert_eq!(has(&inner, "copied-function-body"), code != 0);
        if code != 0 {
            assert_eq!(
                inner["result"]["findings"][0]["evidence"]["peers"],
                json!([["peer.py", "peer", 1]])
            );
        }
        assert_dirty(&case, "a.py");
    }
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_structure_analysis_stage_uses_cross_file_lock_order_and_changed_side() {
    let base =
        "def forward():\n    with alpha_lock:\n        with beta_lock:\n            return 1\n";
    let inverted =
        "def backward():\n    with beta_lock:\n        with alpha_lock:\n            return 2\n";
    for (candidate, code) in [
        (base.replace("return 1", "return 2"), 0),
        (inverted.into(), 1),
    ] {
        let case = case("structure-audit", &[("a.py", base), ("b.py", base)]);
        case.fixture.stage("b.py", &candidate);
        dirty(&case, "b.py");
        let inner = native(&stage(&case, "structure-audit", code), "structure-audit");
        assert_eq!(has(&inner, "inconsistent-lock-order"), code != 0);
        if code != 0 {
            assert_eq!(inner["result"]["findings"][0]["path"], "b.py");
            assert_eq!(inner["result"]["findings"][0]["line"], 3);
        }
        assert_dirty(&case, "b.py");
    }
}

const MANIFEST: &str="[project]\nname='fixture'\nversion='0.0.0'\ndependencies=[]\n[tool.conductor]\npackage_root='src/conductor'\n";
#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_import_analysis_stage_reads_only_granted_pinned_external_provider_observation() {
    for (candidate, code) in [("import os\nvalue = 2\n", 0), ("import requests\n", 1)] {
        let mut case = case(
            "import-declaration",
            &[
                ("pyproject.toml", MANIFEST),
                ("src/conductor/a.py", "import os\nvalue = 1\n"),
            ],
        );
        let (path, bytes) = provider(&mut case, true);
        case.fixture.stage("src/conductor/a.py", candidate);
        dirty(&case, "src/conductor/a.py");
        let report = stage(&case, "import-declaration", code);
        let inner = native(&report, "import-declaration");
        assert_eq!(has(&inner, "undeclared-dependency"), code != 0);
        assert_eq!(
            inner["plan"]["provider_metadata"],
            json!({"path":path,"sha256":format!("{:x}",Sha256::digest(&bytes))})
        );
        for stage in report["stages"].as_array().unwrap() {
            assert!(stage["sandbox"]["read_only"]
                .as_array()
                .unwrap()
                .contains(&json!(path.parent().unwrap())));
        }
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_dirty(&case, "src/conductor/a.py");
    }
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_import_analysis_stage_denies_ungranted_provider_root_and_maps_incomplete_as_critical() {
    let mut case = case(
        "import-declaration",
        &[("pyproject.toml", MANIFEST), ("a.py", "value = 1\n")],
    );
    let (path, bytes) = provider(&mut case, false);
    case.fixture.stage("a.py", "import requests\n");
    incomplete(
        &stage(&case, "import-declaration", 2),
        "bind operator import-provider observation",
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_import_analysis_stage_rejects_hash_drift_in_a_granted_operator_root_as_critical() {
    let mut case = case(
        "import-declaration",
        &[("pyproject.toml", MANIFEST), ("a.py", "value = 1\n")],
    );
    let (path, _) = provider(&mut case, true);
    fs::write(&path, b"{}\n").unwrap();
    case.fixture.stage("a.py", "import requests\n");
    incomplete(&stage(&case, "import-declaration", 2), "digest mismatch");
    assert_eq!(fs::read(path).unwrap(), b"{}\n");
}

#[test]
#[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and installed native FORGE_TEST_BIN"]
fn native_python_analysis_stage_maps_resource_admission_failure_as_critical_incomplete() {
    let case = case("python-ast", &[("a.py", "value = 1\n")]);
    case.fixture
        .stage("a.py", &format!("value = {}1\n", "+".repeat(300)));
    incomplete(&stage(&case, "python-ast", 2), "Python analysis admission");
}

#[test]
fn import_provider_source_and_scratch_paths_fail_grants_admission_before_dry_or_executed_stages() {
    for root in ["repo", "scratch"] {
        let mut case = case_with_delegation(
            "import-declaration",
            &[("pyproject.toml", MANIFEST), ("a.py", "value = 1\n")],
            false,
        );
        let (_, bytes) = provider(&mut case, false);
        let path = case.fixture.root.join(root).join("forbidden-provider.json");
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        case.grants["environment"]["FORGE_IMPORT_PROVIDERS"] = json!(json!({"path":path,
            "sha256":format!("{:x}",Sha256::digest(&bytes))})
        .to_string());
        case.fixture.stage("a.py", "import requests\n");
        let before = case.source_state();
        for execute in [false, true] {
            let mut command = case.command_for("execute", "index", &["import-declaration"]);
            if execute {
                command.arg("--execute");
            }
            let output = run(command);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("outside source and scratch roots"));
            assert!(output.stdout.is_empty());
            // This plain fixture directory is not delegated. Reaching cgroup
            // launch would fail differently and could not produce this admission error.
            assert_eq!(
                fs::read_dir(case.fixture.root.join("cgroup"))
                    .unwrap()
                    .count(),
                0
            );
            assert_eq!(case.source_state(), before);
        }
        assert_eq!(fs::read(&path).unwrap(), bytes);
        if root == "scratch" {
            fs::remove_file(path).unwrap();
        }
        case.assert_preserved(&before);
    }
}
