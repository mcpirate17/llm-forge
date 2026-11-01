#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use git2::{Repository, Signature};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};
const SECRET: &str = "secret-scan";
const NATIVE: &str = "native-source";
fn case() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["programs"] = json!({});
    case.grants["read_only"] = json!([case.fixture.root.join("repo").canonicalize().unwrap()]);
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|text|format!("{text}\n[checks.secret-scan]\nkind='builtin'\nprofiles=['fast']\nclasses=['rust']\nalways=true\n[checks.native-source]\nkind='builtin'\nprofiles=['fast']\nclasses=['docs']\nalways=true\n"));
    case
}
fn invoke(case: &Case, id: &str, execute: bool, code: i32) -> Value {
    let mut command = case.command_for("execute-batch", "index", &[id]);
    if execute {
        command.arg("--execute");
    }
    value(
        forge::process_probe::run(&mut command, Duration::from_secs(12), 1024 * 1024).unwrap(),
        code,
    )
}
fn row(report: &Value) -> &Value {
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    &report["checks"][0]
}
fn findings(report: &Value) -> &[Value] {
    row(report)["result"]["findings"].as_array().unwrap()
}
fn clean(case: &Case, before: &(Vec<u8>, Vec<u8>, String), report: &Value) {
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["unexecuted"], json!([]));
    assert_eq!(row(report)["stages"], json!([]));
    assert_eq!(
        fs::read_dir(case.fixture.root.join("cgroup"))
            .unwrap()
            .count(),
        0
    );
    case.assert_preserved(before);
}
fn commit(case: &Case) {
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let tree_id = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "scan baseline",
            &tree,
            &[&parent],
        )
        .unwrap();
}
fn bytes(case: &Case, path: &str, bytes: &[u8]) {
    let destination = case.fixture.root.join("repo").join(path);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(destination, bytes).unwrap();
    case.fixture.add(path);
}
fn remove_last(case: &Case, path: &str) {
    fs::remove_file(case.fixture.root.join("repo").join(path)).unwrap();
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repository.index().unwrap();
    index.remove_path(Path::new(path)).unwrap();
    index.write().unwrap();
}
fn gitlink_last(case: &Case, path: &str) {
    case.fixture.stage(path, "placeholder\n");
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repository.index().unwrap();
    let mut entry = index.get_path(Path::new(path), 0).unwrap();
    entry.mode = 0o160000;
    entry.id = repository.head().unwrap().target().unwrap();
    entry.file_size = 0;
    index.add(&entry).unwrap();
    index.write().unwrap();
}

#[test]
fn secret_scan_keeps_legacy_scope_skips_nul_text_and_rule_order() {
    let case = case();
    case.staged_policy(|text| text.replace("max_file_bytes=1048576", "max_file_bytes=128"));
    case.fixture.stage("gone.txt", "old clean text\n");
    commit(&case);
    case.fixture.stage(
        "a.txt",
        "token = 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'\r\nAKIAABCDEFGHIJKLMNOP\r",
    );
    bytes(
        &case,
        "b.bin",
        b"\0token = 'BBBBBBBBBBBBBBBBBBBBBBBBBBBBBB'\n",
    );
    bytes(&case, "invalid.bin", b"\xffDO_NOT_LEAK_INVALID_TEXT");
    case.fixture.stage("over.txt", &"x".repeat(129));
    case.fixture.stage("src/clean.rs", "fn clean() {}\n");
    std::os::unix::fs::symlink("a.txt", case.fixture.root.join("repo/link.txt")).unwrap();
    case.fixture.add("link.txt");
    remove_last(&case, "gone.txt");
    let before = case.source_state();
    let report = invoke(&case, SECRET, true, 1);
    let result = &row(&report)["result"];
    assert_eq!(row(&report)["files"], json!(["src/clean.rs"]));
    assert_eq!(result["files"], json!(["a.txt", "b.bin", "src/clean.rs"]));
    let rows = findings(&report);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["rule_id"], "aws-access-key");
    assert_eq!(rows[0]["line"], 2);
    assert_eq!(rows[1]["rule_id"], "generic-api-key");
    assert_eq!(rows[1]["line"], 1);
    assert_eq!(rows[2]["path"], "b.bin");
    assert_eq!(result["metrics"], json!({}));
    assert_eq!(result["tool_version"], "forge-builtin/1");
    for finding in rows {
        assert_eq!(finding["severity"], "critical");
        assert_eq!(finding["fingerprint"].as_str().unwrap().len(), 24);
    }
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("AKIAABCDEFGHIJKLMNOP"));
    assert!(!encoded.contains("AAAAAAAAAAAAAAAAAAAA"));
    clean(&case, &before, &report);
}

#[test]
fn native_scan_reads_safe_symlinks_and_appends_sorted_incomplete_ledger() {
    let case = case();
    commit(&case);
    case.fixture.stage("note.txt", "scheduling scope only\n");
    case.fixture.stage(
        "src/a.c",
        "void f(){\r\n strcpy(dst,src);\r system(cmd);\r\n}\n",
    );
    bytes(&case, "src/z.rs", b"\xffDO_NOT_LEAK_INVALID_TEXT");
    std::os::unix::fs::symlink("a.c", case.fixture.root.join("repo/src/safe.c")).unwrap();
    case.fixture.add("src/safe.c");
    gitlink_last(&case, "src/missing.c");
    let before = case.source_state();
    let report = invoke(&case, NATIVE, true, 1);
    let result = &row(&report)["result"];
    assert_eq!(row(&report)["files"], json!(["note.txt"]));
    assert_eq!(
        result["files"],
        json!(["src/a.c", "src/missing.c", "src/safe.c", "src/z.rs"])
    );
    let rows = findings(&report);
    assert_eq!(rows.len(), 6);
    for finding in &rows[..4] {
        assert_eq!(finding["rule_id"], "unsafe-native-api");
    }
    assert_eq!(rows[0]["line"], 2);
    assert_eq!(rows[1]["line"], 3);
    assert_eq!(rows[2]["path"], "src/safe.c");
    for (finding, path) in rows[4..].iter().zip(["src/missing.c", "src/z.rs"]) {
        assert_eq!(finding["rule_id"], "unreadable-changed-file");
        assert_eq!(finding["path"], path);
        assert_eq!(finding["severity"], "critical");
        assert_eq!(finding["evidence"]["in_change_set"], true);
    }
    let metrics = &result["metrics"];
    assert_eq!(metrics["files_expected"], 4);
    assert_eq!(metrics["files_read"], 2);
    assert_eq!(metrics["files_skipped"], 2);
    assert_eq!(metrics["skipped_files"].as_object().unwrap().len(), 2);
    assert!(metrics["skipped_files"]["src/z.rs"]
        .as_str()
        .unwrap()
        .starts_with("NativeUtf8Error:"));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("DO_NOT_LEAK_INVALID_TEXT"));
    clean(&case, &before, &report);
}

#[test]
fn both_supported_scans_prepare_without_evaluation_and_preserve_source() {
    let case = case();
    case.fixture.stage("src/danger.c", "system(cmd);\n");
    let before = case.source_state();
    for id in [SECRET, NATIVE] {
        let report = invoke(&case, id, false, 0);
        assert_eq!(
            row(&report)["builtin"],
            json!({"implementation":"forge-builtin/1"})
        );
        assert!(row(&report)["result"].is_null());
        clean(&case, &before, &report);
    }
}

#[test]
fn single_read_resource_refusal_is_error_not_clean_scan_for_either_id() {
    let case = case();
    case.staged_policy(|text| text.replace("max_file_bytes=1048576", "max_file_bytes=2097152"));
    bytes(&case, "src/over.c", &vec![b'x'; 1024 * 1024 + 1]);
    let before = case.source_state();
    for id in [SECRET, NATIVE] {
        let report = invoke(&case, id, true, 1);
        assert!(row(&report)["result"].is_null());
        assert!(row(&report)["error"]
            .as_str()
            .unwrap()
            .contains("single read"));
        clean(&case, &before, &report);
    }
}

#[test]
fn native_scan_bounds_aggregate_raw_attempts_before_retaining_more_payload() {
    let case = case();
    commit(&case);
    let text = vec![b'x'; 1024 * 1024];
    for index in 0..33 {
        bytes(&case, &format!("src/bulk_{index:02}.c"), &text);
    }
    let before = case.source_state();
    let report = invoke(&case, NATIVE, true, 1);
    assert!(row(&report)["result"].is_null());
    assert!(row(&report)["error"].as_str().unwrap().contains("32 MiB"));
    clean(&case, &before, &report);
}

#[test]
fn empty_native_handler_scope_still_reports_zero_ledger_counts() {
    let case = case();
    commit(&case);
    case.fixture.stage("note.txt", "only docs changed\n");
    let before = case.source_state();
    let report = invoke(&case, NATIVE, true, 0);
    let result = &row(&report)["result"];
    assert_eq!(result["files"], json!([]));
    assert!(findings(&report).is_empty());
    assert_eq!(
        result["metrics"],
        json!({"files_expected":0,"files_read":0,"files_skipped":0,"skipped_files":{}})
    );
    clean(&case, &before, &report);
}

#[test]
fn secret_scan_explicitly_skips_intentionally_unmaterialized_gitlinks() {
    let case = case();
    commit(&case);
    gitlink_last(&case, "submodule");
    let before = case.source_state();
    let report = invoke(&case, SECRET, true, 0);
    assert_eq!(row(&report)["result"]["status"], "passed");
    assert_eq!(row(&report)["result"]["files"], json!([]));
    assert_eq!(row(&report)["result"]["metrics"], json!({}));
    assert!(findings(&report).is_empty());
    clean(&case, &before, &report);
}

#[test]
fn concurrent_scans_keep_policy_order_and_independent_findings() {
    let case = case();
    commit(&case);
    case.fixture
        .stage("src/both.c", "strcpy(dst, src);\n// AKIAABCDEFGHIJKLMNOP\n");
    let before = case.source_state();
    let mut command = case.command_for("execute-batch", "index", &[NATIVE, SECRET]);
    command.arg("--execute");
    let report = value(
        forge::process_probe::run(&mut command, Duration::from_secs(12), 1024 * 1024).unwrap(),
        1,
    );
    assert_eq!(report["budget"]["admitted_parallelism"], 2);
    assert_eq!(report["unexecuted"], json!([]));
    assert_eq!(report["review_complete"], false);
    let rows = report["checks"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (row, (id, rule)) in rows
        .iter()
        .zip([(SECRET, "aws-access-key"), (NATIVE, "unsafe-native-api")])
    {
        assert_eq!(row["check_id"], id);
        assert!(row["error"].is_null());
        assert_eq!(row["stages"], json!([]));
        assert_eq!(row["result"]["status"], "failed");
        assert_eq!(row["result"]["files"], json!(["src/both.c"]));
        let findings = row["result"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["check_id"], id);
        assert_eq!(findings[0]["rule_id"], rule);
    }
    assert_eq!(
        fs::read_dir(case.fixture.root.join("cgroup"))
            .unwrap()
            .count(),
        0
    );
    case.assert_preserved(&before);
}
