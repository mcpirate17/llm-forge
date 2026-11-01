#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{commit, run, Private};
use candidate_execution_fixture::{value, Case};
use git2::{Oid, Repository};
use serde_json::json;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
#[test]
fn private_detached_context_resolves_alternate_base_and_exact_candidate_tree_without_git_path() {
    let f = Private::new(
        &[("a.c", b"int a;\nint b;\n")],
        &[("a.c", b"int a;\nint changed;\n")],
    );
    let before = f.case.source_state();
    fs::write(f.case.fixture.root.join("repo/a.c"), b"dirty original\n").unwrap();
    let report = f.dry(&["a.c"]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["executed"], false);
    assert_eq!(report["plan"]["candidate_tree_oid"], f.tree);
    assert_eq!(
        report["plan"]["files"][0]["ranges"],
        json!([{"start":2,"end":2}])
    );
    assert_eq!(
        fs::read(f.case.fixture.root.join("repo/a.c")).unwrap(),
        b"dirty original\n"
    );
    let private = Repository::open(&f.git).unwrap();
    let head = private.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 0);
    assert!(
        private.find_commit(Oid::from_str(&f.base).unwrap()).is_ok(),
        "base must resolve through source ODB alternate"
    );
    f.case.assert_preserved(&before);
}
#[test]
fn private_context_snapshot_drift_and_binary_are_not_silently_formatted_whole_file() {
    let f = Private::new(&[], &[("a.c", b"int a;\n")]);
    let before = f.case.source_state();
    fs::write(f.snapshot.path().join("a.c"), b"drifted snapshot\n").unwrap();
    assert!(!run(f.command(&["a.c"])).status.success());
    f.case.assert_preserved(&before);
    let binary = Private::new(&[], &[("a.c", b"int a;\0\n")]);
    let before = binary.case.source_state();
    let output = run(binary.command(&["a.c"]));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("binary"));
    binary.case.assert_preserved(&before);
}
fn staged_case() -> Case {
    let mut case = Case::new(Path::new(support::forge_binary::path()));
    case.staged_policy(|text|text.replace("timeout_seconds=2","timeout_seconds=30").replace("memory_mb=128","memory_mb=1024")
        .replace("[classes]\n", "[classes]\ncfamily=['*.c']\n")
        .replace("\nclasses=['rust']\ncommand=","\nclasses=['cfamily']\ncommand=")
        .replace("command=['worker','echo','literal argument','{files}']","command=['worker','candidate','clang-format','--base','{base}','--tree','{tree}','--execute','--','{files}']")
        .replace("version_command=['worker','echo','fixture-v1']","version_command=['worker','candidate','clang-format','--tool-version']"));
    case.fixture.stage(
        ".clang-format",
        "BasedOnStyle: LLVM\nIndentWidth: 4\nAllowShortFunctionsOnASingleLine: None\n",
    );
    case.fixture.stage(
        "a.c",
        "int old_drift( ){return 1;}\nint selected(void) {\n    return 2;\n}\n",
    );
    commit(&case);
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    case
}
fn provision(case: &mut Case) -> PathBuf {
    case.delegated();
    let tool = PathBuf::from(
        std::env::var_os("FORGE_TEST_CLANG_FORMAT")
            .expect("explicit native clang-format binary required"),
    )
    .canonicalize()
    .unwrap();
    let mut magic = [0; 4];
    fs::File::open(&tool)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, b"\x7fELF");
    case.grants["environment"] = json!({"FORGE_CLANG_FORMAT":tool});
    case.grants["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(tool.parent().unwrap()));
    tool
}
fn real_stage(candidate: &str, expected: i32) {
    let mut case = staged_case();
    let _tool = provision(&mut case);
    case.fixture.stage("a.c", candidate);
    let before = case.source_state();
    let dirty = b"dirty original must survive\n";
    fs::write(case.fixture.root.join("repo/a.c"), dirty).unwrap();
    let output = run({
        let mut command = case.command();
        command.arg("--execute");
        command
    });
    let report = value(output, expected);
    assert_eq!(report["files"], json!(["a.c"]));
    assert_eq!(
        report["result"]["status"],
        if expected == 0 { "passed" } else { "failed" }
    );
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .contains("clang-format version"));
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    assert_eq!(fs::read(case.fixture.root.join("repo/a.c")).unwrap(), dirty);
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_FORMAT and explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn real_stage_sealed_index_passes_with_unmodified_old_formatting_drift() {
    real_stage(
        "int old_drift( ){return 1;}\nint selected(void) {\n    return 3;\n}\n",
        0,
    );
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_FORMAT and explicit delegated FORGE_TEST_CGROUP_PARENT"]
fn real_stage_sealed_index_reports_changed_line_formatting_failure() {
    real_stage(
        "int old_drift( ){return 1;}\nint selected(void) {\nreturn 4;\n}\n",
        1,
    );
}
