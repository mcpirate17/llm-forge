#![cfg(target_os = "linux")]
#[path = "support/candidate_analysis_fixture.rs"]
mod analysis;
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;

use analysis::*;
use candidate_clang_fixture::{assert_code, run};
use git2::{Oid, Repository};
use serde_json::json;
use std::{fs, process::Command};

#[test]
fn dry_plan_binds_captured_trees_and_policy_without_an_interpreter() {
    let f = private(&[("a.py", b"value = 1\n")], &[("a.py", b"value = 2\n")]);
    let before = f.case.source_state();
    let value = report(run(command(&f, "python-ast", &["a.py"], false)), 0);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["check"], "python-ast");
    assert_eq!(value["executed"], false);
    assert!(value["result"].is_null());
    let repo = Repository::open(&f.git).unwrap();
    let base_tree = repo
        .find_commit(Oid::from_str(&f.base).unwrap())
        .unwrap()
        .tree_id()
        .to_string();
    assert_eq!(value["plan"]["base_tree_oid"], base_tree);
    assert_eq!(value["plan"]["candidate_tree_oid"], f.tree);
    assert_eq!(value["plan"]["selected_files"], json!(["a.py"]));
    assert_eq!(value["plan"]["policy_path"], POLICY);
    assert_eq!(value["plan"]["policy_sha256"].as_str().unwrap().len(), 64);
    f.case.assert_preserved(&before);
}

#[test]
fn tool_version_needs_a_check_but_no_git_provider_or_interpreter_context() {
    let directory = tempfile::tempdir().unwrap();
    for check in [
        "config-parse",
        "python-ast",
        "import-declaration",
        "duplicate-function-bodies",
        "structure-audit",
    ] {
        let mut cmd = Command::new(support::forge_binary::path());
        cmd.current_dir(directory.path())
            .env_clear()
            .env("PATH", "")
            .env("HOME", directory.path())
            .env("FORGE_LEDGER_DISABLE", "1")
            .env("GIT_DIR", directory.path().join("absent-git"))
            .env("FORGE_IMPORT_PROVIDERS", "not-json")
            .args(["candidate", "analyze", "--check", check, "--tool-version"]);
        let output = run(cmd);
        assert_code(&output, 0);
        assert!(!output.stdout.is_empty());
    }
    let mut cmd = Command::new(support::forge_binary::path());
    cmd.current_dir(directory.path())
        .env_clear()
        .env("PATH", "")
        .args(["candidate", "analyze", "--tool-version"]);
    assert_code(&run(cmd), 2);
}

#[test]
fn config_errors_and_notebook_outputs_are_native_findings_without_running_cells() {
    let book=json!({"nbformat":4,"cells":[{"cell_type":"code","source":["raise RuntimeError('must never execute')"],
        "outputs":[{"text":"captured output"}],"execution_count":7}]}).to_string();
    let f = private(
        &[],
        &[
            ("bad.toml", b"broken = [\n"),
            ("book.ipynb", book.as_bytes()),
        ],
    );
    let value = execute(&f, "config-parse", &["bad.toml", "book.ipynb"], 1);
    assert!(rule(&value, "bad.toml", "malformed-config").is_some());
    let output = rule(&value, "book.ipynb", "notebook-output").unwrap();
    assert_eq!(output["severity"], "high");
    assert_eq!(output["line"], 1);
    let clean = json!({"nbformat":4,"cells":[{"cell_type":"code","source":["raise RuntimeError()"],
        "outputs":[],"execution_count":null}]})
    .to_string();
    let f = private(
        &[],
        &[
            ("good.toml", b"value = 1\n"),
            ("clean.ipynb", clean.as_bytes()),
        ],
    );
    assert!(findings(&execute(
        &f,
        "config-parse",
        &["good.toml", "clean.ipynb"],
        0
    ))
    .is_empty());
}

#[test]
fn python_changed_line_markers_use_policy_classes_and_leave_unchanged_debt_alone() {
    let f = private(
        &[("a.py", b"# TODO old debt\nvalue = 1\n")],
        &[("a.py", b"# TODO old debt\nvalue = 2\n")],
    );
    assert!(findings(&execute(&f, "python-ast", &["a.py"], 0)).is_empty());
    let marker = b"# TODO newly introduced\nvalue = 1\n";
    let f = private(
        &[],
        &[("ordinary.py", marker), ("specimens/example.py", marker)],
    );
    let value = execute(
        &f,
        "python-ast",
        &["ordinary.py", "specimens/example.py"],
        1,
    );
    assert!(rule(&value, "ordinary.py", "partial-implementation-marker").is_some());
    assert!(rule(
        &value,
        "specimens/example.py",
        "partial-implementation-marker"
    )
    .is_none());
}

#[test]
fn python_hot_and_novel_class_behavior_comes_from_the_captured_policy() {
    let loop_source = b"import re\nfor item in items:\n    re.compile(item)\n";
    let novel = b"value = scaled_dot_product_attention(q, k, v)\n";
    let f = private(
        &[],
        &[
            ("hot_lane/loop.py", loop_source),
            ("ordinary.py", loop_source),
            ("novel_lane/model.py", novel),
        ],
    );
    let value = execute(
        &f,
        "python-ast",
        &["hot_lane/loop.py", "ordinary.py", "novel_lane/model.py"],
        1,
    );
    assert_eq!(
        rule(&value, "hot_lane/loop.py", "repeated-parsing-io").unwrap()["severity"],
        "high"
    );
    assert_eq!(
        rule(&value, "ordinary.py", "repeated-parsing-io").unwrap()["severity"],
        "medium"
    );
    assert_eq!(
        rule(&value, "novel_lane/model.py", "softmax-shaped-fallback").unwrap()["severity"],
        "critical"
    );
}

#[test]
fn invalid_python_is_a_critical_parse_finding_while_admission_limits_are_incomplete() {
    let f = private(&[], &[("bad.py", b"def broken(:\n")]);
    let value = execute(&f, "python-ast", &["bad.py"], 1);
    assert_eq!(
        rule(&value, "bad.py", "python-parse").unwrap()["severity"],
        "critical"
    );
    let source = format!("value = {}1\n", "+".repeat(300));
    let f = private(&[], &[("bounded.py", source.as_bytes())]);
    assert_code(&run(command(&f, "python-ast", &["bounded.py"], true)), 2);
}

#[test]
fn cr_only_git_records_project_to_the_actual_python_finding_line() {
    let source = format!("{}# fallback uses softmax\r", "value = 1\r".repeat(99));
    let f = private(&[], &[("novel_lane/model.py", source.as_bytes())]);
    let value = execute(&f, "python-ast", &["novel_lane/model.py"], 1);
    assert_eq!(
        rule(&value, "novel_lane/model.py", "softmax-shaped-fallback").unwrap()["line"],
        100
    );
}

#[test]
fn unreadable_changed_sources_and_peers_never_return_partial_analysis() {
    for mode in ["python-ast", "duplicate-function-bodies", "structure-audit"] {
        let f = private(&[], &[("broken.py", b"\xff")]);
        let output = run(command(&f, mode, &["broken.py"], true));
        assert_code(&output, 2);
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("UTF-8"));
    }
    for mode in ["duplicate-function-bodies", "structure-audit"] {
        let f = private(
            &[("peer.py", b"\xff")],
            &[("peer.py", b"\xff"), ("changed.py", b"value=1\n")],
        );
        let output = run(command(&f, mode, &["changed.py"], true));
        assert_code(&output, 2);
        assert!(output.stdout.is_empty());
    }
    let f = private(&[], &[("changed.py", b"value=1\n")]);
    fs::remove_file(f.snapshot.path().join("changed.py")).unwrap();
    assert_code(
        &run(command(&f, "structure-audit", &["changed.py"], true)),
        2,
    );
}

#[test]
fn complete_structure_inventory_reports_all_captured_modules() {
    let f = private(
        &[("peer.py", b"value=1\n")],
        &[("peer.py", b"value=1\n"), ("changed.py", b"value=2\n")],
    );
    let value = execute(&f, "structure-audit", &["changed.py"], 0);
    assert_eq!(value["result"]["metrics"]["files_expected"], 2);
    assert_eq!(value["result"]["metrics"]["files_read"], 2);
    assert_eq!(value["result"]["metrics"]["files_skipped"], 0);
    assert_eq!(value["result"]["metrics"]["skipped_files"], json!({}));
}

#[test]
fn import_manifest_scope_uses_captured_layout_and_pinned_operator_observation() {
    let manifest=b"[project]\nname='example'\nversion='0.0.0'\ndependencies=[]\n[project.optional-dependencies]\ndev=['requests']\n[tool.conductor]\npackage_root='src/conductor'\n";
    let f = private(
        &[],
        &[
            ("pyproject.toml", manifest),
            ("src/conductor/runtime.py", b"import requests\n"),
            ("research/client.py", b"import requests\n"),
            ("specimens/client.py", b"import requests\n"),
        ],
    );
    let metadata = provider(&f);
    let mut cmd = command(
        &f,
        "import-declaration",
        &[
            "src/conductor/runtime.py",
            "research/client.py",
            "specimens/client.py",
        ],
        true,
    );
    cmd.env("FORGE_IMPORT_PROVIDERS", &metadata);
    let value = report(run(cmd), 1);
    assert!(rule(
        &value,
        "src/conductor/runtime.py",
        "base-dependency-required"
    )
    .is_some());
    assert!(rule(&value, "research/client.py", "base-dependency-required").is_none());
    assert!(findings(&value)
        .iter()
        .all(|finding| finding["path"] != "specimens/client.py"));
    assert!(value["plan"]["provider_metadata"].is_object());
    assert!(value["plan"]["manifest_paths"]
        .as_array()
        .unwrap()
        .contains(&json!("pyproject.toml")));
}

#[test]
fn import_provider_hash_mismatch_is_incomplete_and_cannot_be_silently_refreshed() {
    let f = private(
        &[],
        &[
            (
                "pyproject.toml",
                b"[project]\nname='x'\nversion='0.0.0'\ndependencies=[]\n",
            ),
            ("a.py", b"import requests\n"),
        ],
    );
    let metadata = provider(&f);
    fs::write(
        f.case
            .fixture
            .root
            .join("operator-providers/observation.json"),
        b"{}\n",
    )
    .unwrap();
    let mut cmd = command(&f, "import-declaration", &["a.py"], true);
    cmd.env("FORGE_IMPORT_PROVIDERS", metadata);
    assert_code(&run(cmd), 2);
}

#[test]
fn duplicate_policy_reads_unchanged_peers_but_requires_a_changed_function_span() {
    let old = long_function("candidate");
    let peer = long_function("peer");
    let outside = format!("{old}\nmarker = 2\n");
    let old_with_marker = format!("{old}\nmarker = 1\n");
    let f = private(
        &[
            ("changed.py", old_with_marker.as_bytes()),
            ("peer.py", peer.as_bytes()),
        ],
        &[
            ("changed.py", outside.as_bytes()),
            ("peer.py", peer.as_bytes()),
        ],
    );
    assert!(findings(&execute(
        &f,
        "duplicate-function-bodies",
        &["changed.py"],
        0
    ))
    .is_empty());
    let renamed = old.replace("def candidate()", "def renamed()");
    let f = private(
        &[("changed.py", old.as_bytes()), ("peer.py", peer.as_bytes())],
        &[
            ("changed.py", renamed.as_bytes()),
            ("peer.py", peer.as_bytes()),
        ],
    );
    let value = execute(&f, "duplicate-function-bodies", &["changed.py"], 1);
    let finding = rule(&value, "changed.py", "copied-function-body").unwrap();
    assert_eq!(finding["line"], 1);
    assert_eq!(
        finding["evidence"]["peers"],
        json!([["peer.py", "peer", 1]])
    );
}

#[test]
fn malformed_unchanged_duplicate_peer_makes_the_inventory_incomplete() {
    let old = long_function("candidate");
    let renamed = old.replace("def candidate()", "def renamed()");
    let f = private(
        &[
            ("changed.py", old.as_bytes()),
            ("bad_peer.py", b"def broken(:\n"),
        ],
        &[
            ("changed.py", renamed.as_bytes()),
            ("bad_peer.py", b"def broken(:\n"),
        ],
    );
    assert_code(
        &run(command(
            &f,
            "duplicate-function-bodies",
            &["changed.py"],
            true,
        )),
        2,
    );
}

#[test]
fn structure_audit_uses_cross_file_peers_and_reports_only_the_changed_lock_side() {
    let forward =
        b"def forward():\n    with alpha_lock:\n        with beta_lock:\n            return 1\n";
    let backward =
        b"def backward():\n    with beta_lock:\n        with alpha_lock:\n            return 2\n";
    let f=private(&[("a.py",forward),("b.py",backward)],
        &[("a.py",forward),("b.py",b"def backward():\n    with beta_lock:\n        with alpha_lock:\n            return 3\n")]);
    let value = execute(&f, "structure-audit", &["b.py"], 1);
    let lock = rule(&value, "b.py", "inconsistent-lock-order").unwrap();
    assert_eq!(lock["severity"], "high");
    assert!(lock["evidence"]["opposite_sites"]
        .as_str()
        .unwrap()
        .contains("a.py:3"));
    assert!(rule(&value, "a.py", "inconsistent-lock-order").is_none());
}

#[test]
fn dirty_original_source_and_policy_cannot_replace_captured_candidate_bytes() {
    let f = private(&[("a.py", b"value = 1\n")], &[("a.py", b"value = 2\n")]);
    let before = f.case.source_state();
    fs::write(f.case.fixture.root.join("repo/a.py"), b"def broken(:\n").unwrap();
    fs::write(
        f.case.fixture.root.join("repo").join(POLICY),
        b"invalid = [\n",
    )
    .unwrap();
    let value = execute(&f, "python-ast", &["a.py"], 0);
    assert!(findings(&value).is_empty());
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo/a.py")).unwrap(),
        b"def broken(:\n"
    );
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo").join(POLICY)).unwrap(),
        b"invalid = [\n"
    );
    f.case.assert_preserved(&before);
}
#[test]
fn import_deleted_and_test_class_files_are_explicitly_excluded_without_providers() {
    let f = private(
        &[("old.py", b"import requests\n")],
        &[("specimens/new.py", b"import unknown_test_dependency\n")],
    );
    let result = report(
        run(command(
            &f,
            "import-declaration",
            &["old.py", "specimens/new.py"],
            true,
        )),
        0,
    );
    assert_eq!(result["plan"]["included_files"], json!([]));
    assert_eq!(
        result["plan"]["excluded_files"],
        json!([
        {"path":"old.py","reason":"deleted"},
        {"path":"specimens/new.py","reason":"test-class"}])
    );
    assert!(result["plan"]["provider_metadata"].is_null());
    assert_eq!(result["result"]["findings"], json!([]));
}

#[test]
fn import_missing_manifest_is_counted_but_malformed_manifest_is_incomplete() {
    let f = private(&[], &[("a.py", b"import requests\n")]);
    let mut cmd = command(&f, "import-declaration", &["a.py"], true);
    cmd.env("FORGE_IMPORT_PROVIDERS", provider(&f));
    let result = report(run(cmd), 0);
    assert_eq!(result["result"]["metrics"]["files_without_manifest"], 1);
    assert_eq!(result["result"]["findings"], json!([]));
    let bad = private(
        &[],
        &[
            ("a.py", b"import requests\n"),
            ("pyproject.toml", b"[project\nname="),
        ],
    );
    let mut cmd = command(&bad, "import-declaration", &["a.py"], true);
    cmd.env("FORGE_IMPORT_PROVIDERS", provider(&bad));
    assert_code(&run(cmd), 2);
}
