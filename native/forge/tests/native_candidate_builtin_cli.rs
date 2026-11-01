#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use git2::{Oid, Repository, Signature};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Output, time::Duration};
const INTEGRITY: &str = "candidate-integrity";
const DEPENDENCY: &str = "dependency-integrity";
fn case() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["programs"] = json!({});
    case.grants["read_only"] = json!([case.fixture.root.join("repo").canonicalize().unwrap()]);
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|text| format!("{text}\n[checks.candidate-integrity]\nkind='builtin'\nprofiles=['fast']\nclasses=['rust']\nalways=true\nrun_on_deletions=true\n[checks.dependency-integrity]\nkind='builtin'\nprofiles=['fast']\nclasses=['dependency']\nalways=true\nrun_on_deletions=true\n"));
    case
}
fn invoke(case: &Case, id: &str, surface: Option<&str>, execute: bool) -> Output {
    let mut command = case.command_for("execute-batch", "index", &[id]);
    if let Some(surface) = surface {
        command.args(["--surface", surface]);
    }
    if execute {
        command.arg("--execute");
    }
    forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap()
}
fn row(report: &Value) -> &Value {
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    &report["checks"][0]
}
fn findings(report: &Value) -> &[Value] {
    row(report)["result"]["findings"].as_array().unwrap()
}
fn rules(report: &Value) -> Vec<&str> {
    findings(report)
        .iter()
        .map(|finding| finding["rule_id"].as_str().unwrap())
        .collect()
}
fn finished(case: &Case, before: &(Vec<u8>, Vec<u8>, String), report: &Value) {
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["unexecuted"], json!([]));
    assert_eq!(row(report)["stages"], json!([]));
    assert_eq!(row(report)["result"]["tool_version"], "forge-builtin/1");
    assert_eq!(
        fs::read_dir(case.fixture.root.join("cgroup"))
            .unwrap()
            .count(),
        0
    );
    for finding in findings(report) {
        let fingerprint = finding["fingerprint"].as_str().unwrap();
        assert_eq!(fingerprint.len(), 24);
        assert!(fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert!(finding["exception_id"].is_null());
        assert_eq!(finding["inherited"], false);
    }
    case.assert_preserved(before);
}
fn commit(case: &Case) -> Oid {
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let oid = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(oid).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &author,
            &author,
            "fixture baseline",
            &tree,
            &[&parent],
        )
        .unwrap()
}
fn remove(case: &Case, path: &str) {
    fs::remove_file(case.fixture.root.join("repo").join(path)).unwrap();
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repository.index().unwrap();
    index.remove_path(Path::new(path)).unwrap();
    index.write().unwrap();
}

#[test]
fn surface_is_explicit_for_scheduled_integrity_and_dependency_needs_none() {
    let case = case();
    let before = case.source_state();
    for execute in [false, true] {
        let output = invoke(&case, INTEGRITY, None, execute);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("surface"));
        case.assert_preserved(&before);
    }
    for surface in ["pre-commit", "post-commit", "ci", "manual"] {
        let report = value(invoke(&case, INTEGRITY, Some(surface), false), 0);
        assert_eq!(
            row(&report)["builtin"],
            json!({"implementation":"forge-builtin/1","unicode_version":"15.0.0"})
        );
        assert!(row(&report)["result"].is_null());
        assert_eq!(row(&report)["stages"], json!([]));
        case.assert_preserved(&before);
    }
    let dependency = value(invoke(&case, DEPENDENCY, None, true), 0);
    assert_eq!(row(&dependency)["result"]["status"], "passed");
    finished(&case, &before, &dependency);
}

#[test]
fn integrity_uses_all_changes_not_filtered_files_without_executable_or_cgroup() {
    let case = case();
    let before = case.source_state();
    fs::write(
        case.fixture.root.join("repo/src/security/test_unit.rs"),
        "dirty unstaged bytes",
    )
    .unwrap();
    let report = value(invoke(&case, INTEGRITY, Some("manual"), true), 0);
    assert_eq!(row(&report)["files"], json!(["src/security/test_unit.rs"]));
    assert_eq!(
        row(&report)["result"]["files"],
        json!(["policies/main.toml", "src/security/test_unit.rs"])
    );
    assert_eq!(
        row(&report)["result"]["metrics"],
        json!({"tree_entries":3,"changes":2})
    );
    assert_eq!(row(&report)["result"]["status"], "passed");
    assert_eq!(row(&report)["builtin"]["unicode_version"], "15.0.0");
    assert_eq!(
        fs::read(case.fixture.root.join("repo/src/security/test_unit.rs")).unwrap(),
        b"dirty unstaged bytes"
    );
    finished(&case, &before, &report);
}

#[test]
fn empty_ci_range_is_a_critical_finding_but_manual_empty_range_passes() {
    let case = case();
    commit(&case);
    let before = case.source_state();
    let ci = value(invoke(&case, INTEGRITY, Some("ci"), true), 1);
    assert_eq!(rules(&ci), ["empty-ci-range"]);
    // Independent literal canonical JSON digest, computed with native sha256sum.
    assert_eq!(findings(&ci)[0]["fingerprint"], "e4954759cdfb699c10373361");
    assert_eq!(findings(&ci)[0]["severity"], "critical");
    assert_eq!(row(&ci)["result"]["files"], json!([]));
    assert_eq!(row(&ci)["result"]["metrics"]["changes"], 0);
    finished(&case, &before, &ci);
    let manual = value(invoke(&case, INTEGRITY, Some("manual"), true), 0);
    assert!(findings(&manual).is_empty());
    finished(&case, &before, &manual);
}

#[test]
fn unicode_full_casefold_three_way_collision_preserves_python_repr_and_order() {
    let case = case();
    for path in ["quote'SS\u{7}.txt", "quote'Ss\u{7}.txt", "quote'ß\u{7}.txt"] {
        case.fixture.stage(path, "same content\n");
    }
    let before = case.source_state();
    let report = value(invoke(&case, INTEGRITY, Some("manual"), true), 1);
    assert_eq!(rules(&report), ["case-collision", "case-collision"]);
    assert_eq!(findings(&report)[0]["path"], "quote'Ss\u{7}.txt");
    assert_eq!(findings(&report)[1]["path"], "quote'ß\u{7}.txt");
    assert_eq!(findings(&report)[0]["message"],"case-colliding tree paths are not portable: \"quote'SS\\x07.txt\" and \"quote'Ss\\x07.txt\"");
    assert_eq!(findings(&report)[1]["message"],"case-colliding tree paths are not portable: \"quote'Ss\\x07.txt\" and \"quote'ß\\x07.txt\"");
    finished(&case, &before, &report);
}

#[test]
fn protected_overwrite_delete_and_rename_keep_paths_and_evidence() {
    let case = case();
    case.staged_policy(|text| {
        text.replace("protected_deletes=[]", "protected_deletes=['protected/*']")
    });
    for (path, body) in [
        ("protected/edit.txt", "edit baseline\n"),
        ("protected/remove.txt", "delete baseline\n"),
        (
            "protected/move.txt",
            "unique rename body remains identical\n",
        ),
    ] {
        case.fixture.stage(path, body);
    }
    commit(&case);
    case.fixture
        .stage("protected/edit.txt", "changed content\n");
    remove(&case, "protected/remove.txt");
    fs::create_dir(case.fixture.root.join("repo/unprotected")).unwrap();
    fs::rename(
        case.fixture.root.join("repo/protected/move.txt"),
        case.fixture.root.join("repo/unprotected/moved.txt"),
    )
    .unwrap();
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repository.index().unwrap();
    index.remove_path(Path::new("protected/move.txt")).unwrap();
    index.add_path(Path::new("unprotected/moved.txt")).unwrap();
    index.write().unwrap();
    let before = case.source_state();
    let report = value(invoke(&case, INTEGRITY, Some("manual"), true), 1);
    assert_eq!(
        rules(&report),
        [
            "protected-overwrite",
            "protected-delete-or-move",
            "protected-delete-or-move"
        ]
    );
    let rows = findings(&report);
    assert_eq!(rows[0]["path"], "protected/edit.txt");
    assert_eq!(rows[1]["path"], "protected/remove.txt");
    assert!(rows[1]["evidence"]["destination"].is_null());
    assert_eq!(rows[2]["path"], "protected/move.txt");
    assert_eq!(rows[2]["evidence"]["destination"], "unprotected/moved.txt");
    assert_eq!(
        row(&report)["result"]["files"],
        json!([
            "protected/edit.txt",
            "protected/remove.txt",
            "unprotected/moved.txt"
        ])
    );
    finished(&case, &before, &report);
}

#[test]
fn binary_addition_uses_binary_limit_and_keeps_fixed_rule_severity() {
    let case = case();
    case.staged_policy(|text| {
        text.replace("max_binary_bytes=67108864", "max_binary_bytes=3")
            .replace(
                "[checks.candidate-integrity]",
                "[checks.candidate-integrity]\nseverity='info'",
            )
    });
    case.fixture.stage("weights.bin", "1234");
    let before = case.source_state();
    let report = value(invoke(&case, INTEGRITY, Some("manual"), true), 1);
    assert_eq!(rules(&report), ["oversized-artifact", "binary-admission"]);
    assert_eq!(
        findings(&report)[0]["evidence"],
        json!({"size_bytes":4,"limit_bytes":3})
    );
    assert_eq!(findings(&report)[0]["severity"], "high");
    assert_eq!(findings(&report)[1]["severity"], "high");
    finished(&case, &before, &report);
}

#[test]
fn safe_symlink_and_gitlink_are_reported_and_unsafe_link_refuses_cleanly() {
    let case = case();
    let root = case.fixture.root.join("repo");
    std::os::unix::fs::symlink("baseline.txt", root.join("link.txt")).unwrap();
    case.fixture.add("link.txt");
    case.fixture.stage("module", "placeholder\n");
    let repository = Repository::open(&root).unwrap();
    let mut index = repository.index().unwrap();
    let mut entry = index.get_path(Path::new("module"), 0).unwrap();
    entry.mode = 0o160000;
    entry.id = repository.head().unwrap().target().unwrap();
    entry.file_size = 0;
    index.add(&entry).unwrap();
    index.write().unwrap();
    let before = case.source_state();
    let report = value(invoke(&case, INTEGRITY, Some("manual"), true), 1);
    assert_eq!(
        rules(&report),
        [
            "symlink-admission",
            "submodule-admission",
            "incomplete-snapshot"
        ]
    );
    assert_eq!(findings(&report)[0]["evidence"]["target"], "baseline.txt");
    assert_eq!(
        findings(&report)[1]["evidence"]["gitlink_oid"],
        entry.id.to_string()
    );
    finished(&case, &before, &report);
    fs::remove_file(root.join("link.txt")).unwrap();
    let outside = case.fixture.root.join("outside-owned.txt");
    fs::write(&outside, "private sentinel").unwrap();
    std::os::unix::fs::symlink("../outside-owned.txt", root.join("link.txt")).unwrap();
    case.fixture.add("link.txt");
    let before = case.source_state();
    assert!(!invoke(&case, INTEGRITY, Some("manual"), true)
        .status
        .success());
    assert_eq!(fs::read(outside).unwrap(), b"private sentinel");
    case.assert_preserved(&before);
}

#[test]
fn dependency_pairing_uses_staged_tree_including_safe_lock_symlink() {
    let case = case();
    case.fixture
        .stage("pkg/Cargo.toml", "[package]\nname='fixture'\n");
    let before = case.source_state();
    let missing = value(invoke(&case, DEPENDENCY, None, true), 1);
    assert_eq!(rules(&missing), ["missing-lockfile"]);
    assert_eq!(findings(&missing)[0]["path"], "pkg/Cargo.toml");
    assert_eq!(findings(&missing)[0]["severity"], "critical");
    finished(&case, &before, &missing);
    // A dirty untracked lock must not fill a selected-tree hole.
    fs::write(case.fixture.root.join("repo/pkg/Cargo.lock"), "dirty lock").unwrap();
    let before = case.source_state();
    let still_missing = value(invoke(&case, DEPENDENCY, None, true), 1);
    assert_eq!(rules(&still_missing), ["missing-lockfile"]);
    finished(&case, &before, &still_missing);
    fs::remove_file(case.fixture.root.join("repo/pkg/Cargo.lock")).unwrap();
    case.fixture.stage("locks/stable.lock", "staged lock\n");
    std::os::unix::fs::symlink(
        "../locks/stable.lock",
        case.fixture.root.join("repo/pkg/Cargo.lock"),
    )
    .unwrap();
    case.fixture.add("pkg/Cargo.lock");
    let before = case.source_state();
    let pass = value(invoke(&case, DEPENDENCY, None, true), 0);
    assert_eq!(row(&pass)["result"]["status"], "passed");
    assert!(findings(&pass).is_empty());
    assert_eq!(
        row(&pass)["result"]["metrics"],
        json!({"dependency_files":2})
    );
    finished(&case, &before, &pass);
}

#[test]
fn deleted_lock_is_not_snapshot_evidence_and_dependency_scope_ignores_class_filter() {
    let case = case();
    case.fixture
        .stage("pkg/Cargo.toml", "[package]\nname='before'\n");
    case.fixture.stage("pkg/Cargo.lock", "old lock\n");
    commit(&case);
    case.fixture
        .stage("pkg/Cargo.toml", "[package]\nname='after'\n");
    case.fixture
        .stage("notes/readme.txt", "unrelated document\n");
    case.staged_policy(|text| {
        text.replace(
        "[checks.dependency-integrity]\nkind='builtin'\nprofiles=['fast']\nclasses=['dependency']",
        "[checks.dependency-integrity]\nkind='builtin'\nprofiles=['fast']\nclasses=['rust']")
    });
    // Fixture retains a Repository with a cached index. Perform this separate
    // index mutation last so later Fixture::stage calls cannot restore the lock.
    remove(&case, "pkg/Cargo.lock");
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    assert!(
        repository
            .index()
            .unwrap()
            .get_path(Path::new("pkg/Cargo.lock"), 0)
            .is_none(),
        "fixture must stage deletion, not merely remove working bytes"
    );
    assert!(
        repository
            .head()
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .get_path(Path::new("pkg/Cargo.lock"))
            .is_ok(),
        "fixture baseline must contain the lock"
    );
    assert!(!case.fixture.root.join("repo/pkg/Cargo.lock").exists());
    let before = case.source_state();
    let report = value(invoke(&case, DEPENDENCY, None, true), 1);
    assert_eq!(row(&report)["files"], json!([]));
    assert_eq!(
        row(&report)["result"]["files"],
        json!(["pkg/Cargo.lock", "pkg/Cargo.toml"])
    );
    assert_eq!(
        row(&report)["result"]["metrics"],
        json!({"dependency_files":2})
    );
    assert_eq!(rules(&report), ["missing-lockfile"]);
    assert_eq!(findings(&report)[0]["path"], "pkg/Cargo.toml");
    assert_eq!(row(&report)["result"]["status"], "failed");
    finished(&case, &before, &report);
}
