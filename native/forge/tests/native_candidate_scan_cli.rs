#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{assert_code, run, Private};
use git2::{ObjectType, Oid};
use serde_json::{json, Value};
use std::{fs, process::Output};
fn scan(f: &Private, check: &str, execute: bool, files: &[&str]) -> Output {
    let mut options = vec!["--check", check];
    if execute {
        options.push("--execute");
    }
    let mut command = f.tool_command_options("scan", &options, files);
    command
        .env("PYO3_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_PYTHON", "/forbidden-python");
    run(command)
}
fn report(output: Output, code: i32, check: &str, executed: bool) -> Value {
    assert_code(&output, code);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["check"], check);
    assert_eq!(value["executed"], executed);
    assert!(value["plan"]["files"].is_array());
    assert!(value["excluded"].is_array());
    assert!(value["hot_paths"].is_array());
    assert!(value["source_bytes"].is_u64());
    value
}
fn has(value: &Value, rule: &str, line: u64, end: u64) -> bool {
    value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["rule"] == rule && row["line"] == line && row["end_line"] == end)
}
#[test]
fn every_scan_version_probe_needs_neither_git_nor_an_interpreter() {
    let host = support::Host::new();
    for check in [
        "style-scan",
        "rust-style-scan",
        "hot-loop-scan",
        "test-value-scan",
        "verbosity-scan",
    ] {
        let mut command = host.command();
        command
            .env_clear()
            .env("PATH", "")
            .env("HOME", &host.0)
            .env("FORGE_LEDGER_DISABLE", "1")
            .env("PYO3_PYTHON", "/forbidden-python")
            .args(["candidate", "scan", "--repo"])
            .arg(&host.0)
            .args(["--check", check, "--tool-version"]);
        let output = run(command);
        assert_code(&output, 0);
        assert!(!String::from_utf8(output.stdout).unwrap().trim().is_empty());
    }
}
#[test]
fn style_scan_scopes_to_finding_line_and_dry_plan_does_not_emit_findings() {
    let base = b"# Now we walk the list and total it up.\nx = 1\n";
    let target = b"# Now we walk the list and total it up.\nx = 2\n";
    let f = Private::new(&[("a.py", base)], &[("a.py", target)]);
    let before = f.case.source_state();
    fs::write(
        f.case.fixture.root.join("repo/a.py"),
        b"invalid dirty source\n",
    )
    .unwrap();
    let dry = report(
        scan(&f, "style-scan", false, &["a.py"]),
        0,
        "style-scan",
        false,
    );
    assert_eq!(dry["scanned_file_count"], 0);
    assert_eq!(dry["source_bytes"], target.len());
    assert!(dry["findings"].is_null());
    assert_eq!(
        dry["plan"]["files"][0]["ranges"],
        json!([{"start":2,"end":2}])
    );
    let actual = report(
        scan(&f, "style-scan", true, &["a.py"]),
        0,
        "style-scan",
        true,
    );
    assert_eq!(actual["scanned_file_count"], 1);
    assert_eq!(actual["source_bytes"], target.len());
    assert_eq!(actual["findings"], json!([]));
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo/a.py")).unwrap(),
        b"invalid dirty source\n"
    );
    f.case.assert_preserved(&before);
    let changed = Private::new(
        &[("a.py", b"# Domain invariant.\nx = 1\n")],
        &[("a.py", base)],
    );
    let before = changed.case.source_state();
    let actual = report(
        scan(&changed, "style-scan", true, &["a.py"]),
        1,
        "style-scan",
        true,
    );
    assert!(has(&actual, "comment/effort-narrative", 1, 1));
    changed.case.assert_preserved(&before);
}
#[test]
fn test_value_and_verbosity_keep_function_findings_when_only_body_changes() {
    let f = Private::new(
        &[("tests/test_a.py", b"def test_a():\n    x = 1\n    run(x)\n")],
        &[("tests/test_a.py", b"def test_a():\n    x = 2\n    run(x)\n")],
    );
    let before = f.case.source_state();
    let actual = report(
        scan(&f, "test-value-scan", true, &["tests/test_a.py"]),
        1,
        "test-value-scan",
        true,
    );
    assert_eq!(
        actual["plan"]["files"][0]["ranges"],
        json!([{"start":2,"end":2}])
    );
    assert!(has(&actual, "test/no-assertion", 1, 3));
    assert_eq!(actual["scanned_file_count"], 1);
    f.case.assert_preserved(&before);
    let comments: String = (0..9)
        .map(|i| format!("    # step {i} of the plan\n"))
        .collect();
    let base = format!("def f(x):\n{comments}    return x\n");
    let target = format!("def f(x):\n{comments}    return x + 1\n");
    let f = Private::new(&[("a.py", base.as_bytes())], &[("a.py", target.as_bytes())]);
    let before = f.case.source_state();
    let actual = report(
        scan(&f, "verbosity-scan", true, &["a.py"]),
        1,
        "verbosity-scan",
        true,
    );
    assert_eq!(
        actual["plan"]["files"][0]["ranges"],
        json!([{"start":11,"end":11}])
    );
    assert!(has(&actual, "comment/comment-heavy-function", 1, 11));
    f.case.assert_preserved(&before);
}
#[test]
fn rust_scan_uses_native_rule_and_suffix_skips_are_explicit() {
    let f = Private::new(
        &[("a.rs", b"fn f() {\n    Some(1).unwrap();\n}\n")],
        &[
            ("a.rs", b"fn f() {\n    Some(2).unwrap();\n}\n"),
            ("plain.txt", b"not Python or Rust\n"),
        ],
    );
    let before = f.case.source_state();
    let actual = report(
        scan(&f, "rust-style-scan", true, &["a.rs", "plain.txt"]),
        1,
        "rust-style-scan",
        true,
    );
    assert_eq!(actual["scanned_file_count"], 1);
    assert!(has(&actual, "failure/rust-unwrap", 2, 2));
    let excluded = actual["excluded"].as_array().unwrap();
    assert!(
        excluded
            .iter()
            .any(|entry| entry["path"] == "plain.txt"
                && !entry["reason"].as_str().unwrap().is_empty())
    );
    let skipped = report(
        scan(&f, "style-scan", true, &["a.rs", "plain.txt"]),
        0,
        "style-scan",
        true,
    );
    assert_eq!(skipped["scanned_file_count"], 0);
    assert_eq!(skipped["findings"], json!([]));
    for path in ["a.rs", "plain.txt"] {
        assert!(skipped["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == path && entry["reason"].is_string()));
    }
    f.case.assert_preserved(&before);
}
#[test]
fn deleted_and_pure_deletion_changes_never_scan_whole_files() {
    let f = Private::new(
        &[("gone.py", b"x = 1\n"), ("trim.py", b"x = 1\ny = 2\n")],
        &[("trim.py", b"x = 1\n")],
    );
    let before = f.case.source_state();
    let actual = report(
        scan(&f, "style-scan", true, &["trim.py", "gone.py"]),
        0,
        "style-scan",
        true,
    );
    assert_eq!(actual["scanned_file_count"], 0);
    assert_eq!(actual["findings"], json!([]));
    for (path, scope) in [("gone.py", "deleted"), ("trim.py", "no-changed-lines")] {
        assert!(actual["plan"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == path && file["scope"] == scope));
        assert!(actual["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == path && entry["reason"].is_string()));
    }
    f.case.assert_preserved(&before);
}
#[test]
fn syntax_drift_and_unselected_snapshot_files_are_incomplete() {
    let invalid = Private::new(&[("a.py", b"x = 1\n")], &[("a.py", b"def broken(:\n")]);
    let before = invalid.case.source_state();
    assert_code(&scan(&invalid, "style-scan", true, &["a.py"]), 2);
    invalid.case.assert_preserved(&before);
    let f = Private::new(&[("a.py", b"x = 1\n")], &[("a.py", b"x = 2\n")]);
    let before = f.case.source_state();
    assert_code(&scan(&f, "style-scan", true, &["baseline.txt"]), 2);
    fs::write(f.snapshot.path().join("a.py"), b"x = 3\n").unwrap();
    assert_code(&scan(&f, "style-scan", true, &["a.py"]), 2);
    assert_code(&scan(&f, "not-a-check", true, &["a.py"]), 2);
    f.case.assert_preserved(&before);
}
const HOT_BASE: &str =
    "def f(x: Tensor):\n    for i in range(len(x)):\n        print(x[i].item())\n";

#[test]
fn captured_hot_config_absence_and_content_are_verified() {
    let f = Private::new(&[], &[("models/a.py", HOT_BASE.as_bytes())]);
    fs::write(
        f.snapshot.path().join("pyproject.toml"),
        b"[tool.forge.hot_paths]\npaths=['models']\n",
    )
    .unwrap();
    assert_code(&scan(&f, "hot-loop-scan", true, &["models/a.py"]), 2);
    let f = Private::new(
        &[],
        &[
            ("models/a.py", HOT_BASE.as_bytes()),
            (
                "pyproject.toml",
                b"[tool.forge.hot_paths]\npaths=['models']\n",
            ),
        ],
    );
    fs::write(
        f.snapshot.path().join("pyproject.toml"),
        b"[project]\nname='drift'\n",
    )
    .unwrap();
    assert_code(&scan(&f, "hot-loop-scan", true, &["models/a.py"]), 2);
}

#[test]
fn style_includes_fallback_detector_and_refuses_result_overflow() {
    let f = Private::new(
        &[],
        &[("a.py", b"try:\n    risky()\nexcept ValueError:\n    pass\n")],
    );
    let actual = report(
        scan(&f, "style-scan", true, &["a.py"]),
        1,
        "style-scan",
        true,
    );
    assert!(has(&actual, "failure/silent-fallback", 3, 3));
    let source: String = (0..4097)
        .map(|i| format!("def unfinished_{i}(value):\n    pass\n"))
        .collect();
    let f = Private::new(&[], &[("a.py", source.as_bytes())]);
    let output = scan(&f, "style-scan", true, &["a.py"]);
    assert_code(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("findings exceed 4096"));
    assert!(output.stdout.is_empty());
}

#[test]
fn duplicate_symlink_and_oversized_source_fail_admission() {
    let f = Private::new(&[], &[("a.py", b"x = 1\n")]);
    assert_code(&scan(&f, "style-scan", true, &["a.py", "a.py"]), 2);
    fs::remove_file(f.snapshot.path().join("a.py")).unwrap();
    std::os::unix::fs::symlink(
        f.case.fixture.root.join("repo/a.py"),
        f.snapshot.path().join("a.py"),
    )
    .unwrap();
    assert_code(&scan(&f, "style-scan", true, &["a.py"]), 2);
    let oversized = format!("# {}\n", "x".repeat(1024 * 1024));
    let f = Private::new(&[], &[("a.py", oversized.as_bytes())]);
    assert_code(&scan(&f, "style-scan", true, &["a.py"]), 2);
}
const HOT_TARGET: &str =
    "def f(x: Tensor):\n    for j in range(len(x)):\n        print(x[j].item())\n";
#[test]
fn hot_loop_uses_captured_pyproject_and_reports_its_git_blob() {
    let config = "[tool.forge.hot_paths]\npaths=['models']\n";
    let f = Private::new(
        &[("models/a.py", HOT_BASE.as_bytes())],
        &[
            ("models/a.py", HOT_TARGET.as_bytes()),
            ("pyproject.toml", config.as_bytes()),
        ],
    );
    let before = f.case.source_state();
    fs::write(
        f.case.fixture.root.join("repo/pyproject.toml"),
        "invalid dirty settings\n",
    )
    .unwrap();
    let actual = report(
        scan(&f, "hot-loop-scan", true, &["models/a.py"]),
        1,
        "hot-loop-scan",
        true,
    );
    assert_eq!(actual["hot_paths"], json!(["models"]));
    assert_eq!(
        actual["config_blob_oid"],
        Oid::hash_object(ObjectType::Blob, config.as_bytes())
            .unwrap()
            .to_string()
    );
    assert_eq!(actual["scanned_file_count"], 1);
    assert!(has(&actual, "perf/python-tensor-loop", 2, 2));
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo/pyproject.toml")).unwrap(),
        b"invalid dirty settings\n"
    );
    f.case.assert_preserved(&before);
}
#[test]
fn absent_hot_scope_is_explicit_and_malformed_captured_settings_fail() {
    for config in [None, Some("[project]\nname='fixture'\nversion='0.1.0'\n")] {
        let mut target: Vec<(&str, &[u8])> = vec![("models/a.py", HOT_TARGET.as_bytes())];
        if let Some(config) = config {
            target.push(("pyproject.toml", config.as_bytes()));
        }
        let f = Private::new(&[("models/a.py", HOT_BASE.as_bytes())], &target);
        let before = f.case.source_state();
        let actual = report(
            scan(&f, "hot-loop-scan", true, &["models/a.py"]),
            0,
            "hot-loop-scan",
            true,
        );
        assert_eq!(actual["hot_paths"], json!([]));
        assert_eq!(actual["scanned_file_count"], 0);
        assert_eq!(actual["findings"], json!([]));
        assert_eq!(actual["config_blob_oid"].is_null(), config.is_none());
        assert!(!actual["excluded"].as_array().unwrap().is_empty());
        f.case.assert_preserved(&before);
    }
    let f = Private::new(
        &[("models/a.py", HOT_BASE.as_bytes())],
        &[
            ("models/a.py", HOT_TARGET.as_bytes()),
            (
                "pyproject.toml",
                b"[tool.forge.hot_paths]\npaths='models'\n",
            ),
        ],
    );
    let before = f.case.source_state();
    assert_code(&scan(&f, "hot-loop-scan", true, &["models/a.py"]), 2);
    f.case.assert_preserved(&before);
}
#[test]
fn rust_test_file_exclusions_keep_production_test_prefix_names() {
    let source = b"fn f() { Some(1).unwrap(); }\n";
    let production = "src/security/test_unit.rs";
    let excluded = ["tests/x.rs", "src/unit_test.rs", "benches/x.rs"];
    let f = Private::new(
        &[],
        &[
            (production, source),
            (excluded[0], source),
            (excluded[1], source),
            (excluded[2], source),
        ],
    );
    let before = f.case.source_state();
    let selected = [production, excluded[0], excluded[1], excluded[2]];
    let actual = report(
        scan(&f, "rust-style-scan", true, &selected),
        1,
        "rust-style-scan",
        true,
    );
    assert_eq!(actual["scanned_file_count"], 1);
    assert!(has(&actual, "failure/rust-unwrap", 1, 1));
    assert!(actual["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["path"] == production));
    for path in excluded {
        assert!(actual["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == path && entry["reason"].is_string()));
    }
    f.case.assert_preserved(&before);
}
