#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const CHECK: &str = "cargo-audit";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn bundle() -> Value {
    json!({"schema_version":1,"captured_at":now(),"advisory_commit":"a".repeat(40),
        "advisories":[{"path":"crates/example/RUSTSEC-2021-0001.md","text":
            "```toml\n[advisory]\nid = \"RUSTSEC-2021-0001\"\npackage = \"example\"\ndate = \"2021-01-01\"\n[versions]\npatched = [\">= 2.0.0\"]\n```\n\n# Fixture vulnerability 雪\n\nOwned fixture advisory.\n"}],
        "registry":[{"name":"example","version":"1.0.0","checksum":"1".repeat(64),"yanked":false},
            {"name":"example","version":"2.0.0","checksum":"2".repeat(64),"yanked":false}]})
}
fn lock(version: &str) -> String {
    let checksum = if version == "1.0.0" { "1" } else { "2" }.repeat(64);
    format!("version = 3\n\n[[package]]\nname = \"example\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n")
}
fn new_case() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["programs"] = json!({});
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|text| {
        text.replace("[checks.zeta]", "[checks.cargo-audit]")
            .replace("kind='command'", "kind='builtin'\ncache=false")
            .replace(
                "\nclasses=['rust']\ncommand=",
                "\nclasses=['rust_dependency']\ncommand=",
            )
            .replace(
                "command=['worker','echo','literal argument','{files}']\n",
                "",
            )
            .replace("version_command=['worker','echo','fixture-v1']\n", "")
    });
    case
}
fn stage_lock(case: &Case, path: &str, version: &str) {
    case.fixture.stage(path, &lock(version));
}
fn pin(case: &mut Case, bundle: &Value) -> PathBuf {
    let path = case.fixture.root.join("audit-bundle.json");
    let bytes = serde_json::to_vec(bundle).unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    case.grants["cargo_audit_evidence"] =
        json!({"path":path,"sha256":format!("{:x}",Sha256::digest(&bytes)),"max_age_seconds":3600});
    path
}
fn invoke(case: &Case, execute: bool) -> std::process::Output {
    let mut command = case.command_for("execute-batch", "index", &[CHECK]);
    if execute {
        command.arg("--execute");
    }
    forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap()
}
fn row(report: &Value) -> &Value {
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    &report["checks"][0]
}
fn finish(case: &Case, before: &(Vec<u8>, Vec<u8>, String), report: &Value) {
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["unexecuted"], json!([]));
    assert_eq!(row(report)["check_id"], CHECK);
    assert_eq!(row(report)["stages"], json!([]));
    assert_eq!(
        fs::read_dir(case.fixture.root.join("cgroup"))
            .unwrap()
            .count(),
        0
    );
    case.assert_preserved(before);
}
fn execution_error(case: &Case, needle: &str) {
    let before = case.source_state();
    let report = value(invoke(case, true), 1);
    assert_eq!(report["success"], false);
    assert!(row(&report)["result"].is_null());
    assert!(
        row(&report)["error"].as_str().unwrap().contains(needle),
        "{report}"
    );
    finish(case, &before, &report);
}
fn findings(report: &Value) -> &[Value] {
    row(report)["result"]["findings"].as_array().unwrap()
}

#[test]
fn vulnerability_clean_and_yanked_are_native_observations_without_tools_or_cgroup() {
    for (version, yanked, rule, code) in [
        ("1.0.0", false, Some("vulnerable-dependency"), 1),
        ("2.0.0", false, None, 0),
        ("2.0.0", true, Some("yanked-dependency"), 1),
    ] {
        let mut case = new_case();
        stage_lock(&case, "Cargo.lock", version);
        let mut evidence = bundle();
        evidence["registry"][1]["yanked"] = json!(yanked);
        pin(&mut case, &evidence);
        let before = case.source_state();
        if rule.is_none() {
            let dry = value(invoke(&case, false), 0);
            assert_eq!(dry["executed"], false);
            assert!(row(&dry)["result"].is_null());
            finish(&case, &before, &dry);
        }
        let report = value(invoke(&case, true), code);
        assert!(row(&report)["error"].is_null());
        assert_eq!(row(&report)["result"]["tool_version"], "forge-builtin/1");
        assert_eq!(row(&report)["result"]["command"], json!([]));
        if let Some(rule) = rule {
            assert_eq!(findings(&report).len(), 1);
            assert_eq!(findings(&report)[0]["rule_id"], rule);
            assert_eq!(findings(&report)[0]["path"], "Cargo.lock");
            assert_eq!(
                findings(&report)[0]["fingerprint"].as_str().unwrap().len(),
                24
            );
            assert!(findings(&report)[0]["exception_id"].is_null());
            assert_eq!(findings(&report)[0]["inherited"], false);
        } else {
            assert!(findings(&report).is_empty());
        }
        assert_eq!(
            row(&report)["result"]["metrics"]["advisory_commit"],
            "a".repeat(40)
        );
        finish(&case, &before, &report);
    }
}

#[test]
fn real_registry_coverage_and_checksum_are_required() {
    for wrong_checksum in [false, true] {
        let mut case = new_case();
        stage_lock(&case, "Cargo.lock", "2.0.0");
        let mut evidence = bundle();
        if wrong_checksum {
            evidence["registry"][1]["checksum"] = json!("3".repeat(64));
        } else {
            evidence["registry"] = json!([]);
        }
        pin(&mut case, &evidence);
        execution_error(
            &case,
            if wrong_checksum {
                "checksum mismatch"
            } else {
                "missing crates.io registry evidence"
            },
        );
    }
}

#[test]
fn evidence_exact_hash_and_freshness_are_enforced() {
    for mode in ["hash", "stale", "future"] {
        let mut case = new_case();
        stage_lock(&case, "Cargo.lock", "2.0.0");
        let mut evidence = bundle();
        if mode == "stale" {
            evidence["captured_at"] = json!(now() - 7200);
        }
        if mode == "future" {
            evidence["captured_at"] = json!(now() + 7200);
        }
        pin(&mut case, &evidence);
        if mode == "hash" {
            case.grants["cargo_audit_evidence"]["sha256"] = json!("0".repeat(64));
        }
        execution_error(
            &case,
            if mode == "hash" {
                "SHA-256 mismatch"
            } else {
                "stale or future"
            },
        );
    }
}

#[test]
fn evidence_inside_repo_symlink_writable_or_hardlinked_cannot_supply_authority() {
    for mode in ["in-repo", "symlink", "writable", "hardlink"] {
        let mut case = new_case();
        stage_lock(&case, "Cargo.lock", "2.0.0");
        let path = pin(&mut case, &bundle());
        match mode {
            "in-repo" => {
                let to = case.fixture.root.join("repo/evidence.json");
                fs::copy(&path, &to).unwrap();
                fs::set_permissions(&to, fs::Permissions::from_mode(0o600)).unwrap();
                case.grants["cargo_audit_evidence"]["path"] = json!(to);
            }
            "symlink" => {
                let to = case.fixture.root.join("alias.json");
                std::os::unix::fs::symlink(&path, &to).unwrap();
                case.grants["cargo_audit_evidence"]["path"] = json!(to);
            }
            "writable" => fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap(),
            _ => fs::hard_link(&path, case.fixture.root.join("second-link.json")).unwrap(),
        }
        execution_error(
            &case,
            match mode {
                "in-repo" => "outside the source",
                "symlink" => "symlinks",
                _ => "owned private regular single-link",
            },
        );
    }
}

#[test]
fn foreign_uid_evidence_is_rejected_when_fixture_can_assign_ownership() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let mut case = new_case();
    stage_lock(&case, "Cargo.lock", "2.0.0");
    let path = pin(&mut case, &bundle());
    std::os::unix::fs::chown(&path, Some(12345), None).unwrap();
    execution_error(&case, "owned private regular single-link");
}

#[test]
fn staged_vulnerability_ignores_candidate_suppression_config_and_dirty_worktree() {
    let mut case = new_case();
    stage_lock(&case, "Cargo.lock", "1.0.0");
    case.fixture.stage(
        ".cargo/audit.toml",
        "[advisories]\nignore = [\"RUSTSEC-2021-0001\"]\n",
    );
    pin(&mut case, &bundle());
    let before = case.source_state();
    fs::write(case.fixture.root.join("repo/Cargo.lock"), lock("2.0.0")).unwrap();
    fs::write(
        case.fixture.root.join("repo/.cargo/audit.toml"),
        "ignore everything dirty",
    )
    .unwrap();
    let report = value(invoke(&case, true), 1);
    assert_eq!(findings(&report)[0]["rule_id"], "vulnerable-dependency");
    assert_eq!(
        fs::read_to_string(case.fixture.root.join("repo/Cargo.lock")).unwrap(),
        lock("2.0.0")
    );
    assert_eq!(
        fs::read_to_string(case.fixture.root.join("repo/.cargo/audit.toml")).unwrap(),
        "ignore everything dirty"
    );
    finish(&case, &before, &report);
}

#[test]
fn multi_lock_ownership_and_observations_are_sorted_and_deduplicated() {
    let mut case = new_case();
    for path in ["z/Cargo.lock", "a/Cargo.lock"] {
        stage_lock(&case, path, "2.0.0");
    }
    case.fixture.stage(
        "a/Cargo.toml",
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    );
    pin(&mut case, &bundle());
    let before = case.source_state();
    let report = value(invoke(&case, true), 0);
    let metrics = &row(&report)["result"]["metrics"];
    assert_eq!(
        metrics["owner_plan"]["lockfiles"],
        json!(["a/Cargo.lock", "z/Cargo.lock"])
    );
    assert_eq!(
        metrics["owner_plan"]["ownership"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        metrics["audits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["lockfile"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a/Cargo.lock", "z/Cargo.lock"]
    );
    assert_eq!(
        row(&report)["result"]["files"],
        json!(["a/Cargo.lock", "a/Cargo.toml", "z/Cargo.lock"])
    );
    finish(&case, &before, &report);
}

#[test]
fn missing_evidence_or_missing_owner_fail_but_no_selected_files_skip_without_evidence() {
    let case = new_case();
    let before = case.source_state();
    let report = value(invoke(&case, true), 0);
    assert_eq!(row(&report)["skipped"], true);
    assert!(row(&report)["result"].is_null());
    finish(&case, &before, &report);
    stage_lock(&case, "Cargo.lock", "2.0.0");
    execution_error(&case, "cargo_audit_evidence");
    let mut unowned = new_case();
    unowned
        .fixture
        .stage("Cargo.toml", "[package]\nname='unowned'\nversion='0.1.0'\n");
    pin(&mut unowned, &bundle());
    execution_error(&unowned, "no owning Cargo.lock");
}

#[test]
fn cacheable_builtin_audit_policy_is_refused_before_evidence_or_execution() {
    let case = new_case();
    case.staged_policy(|text| text.replace("\ncache=false", "\ncache=true"));
    let before = case.source_state();
    let output = invoke(&case, true);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("checks.cargo-audit.cache must be false")
    );
    case.assert_preserved(&before);
}
