#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use git2::{Oid, Repository, Signature};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

const COMMAND: &str = "command=['worker','--enable=warning,performance,portability','--error-exitcode=1','--inline-suppr','--language=c++','--std=c++17','--file-list={changed_files_file}']";
fn case() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|text| text.replace("\nclasses=['rust']\ncommand=", "\nclasses=['native']\npath_suffixes=['.c','.cc','.cpp','.cxx','.h','.hh','.hpp','.hxx','.cu','.cuh']\nfile_input='cppcheck-list'\ncommand=")
        .replace("command=['worker','echo','literal argument','{files}']", COMMAND)
        .replace("version_command=['worker','echo','fixture-v1']", "version_command=['worker','--version']"));
    case
}
fn invoke(case: &Case, batch: bool, execute: bool) -> std::process::Output {
    let mut command = if batch {
        case.command_for("execute-batch", "index", &[])
    } else {
        case.command()
    };
    if execute {
        command.arg("--execute");
    }
    forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap()
}
fn prepared(case: &Case, report: &Value, files: Value) {
    assert_eq!(report["files"], files);
    assert_eq!(report["executed"], false);
    assert!(report["result"].is_null());
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(stages[0]["stage"], "version");
    assert_eq!(stages[0]["command"], json!(["worker", "--version"]));
    assert_eq!(stages[1]["stage"], "command");
    let argv = stages[1]["command"].as_array().unwrap();
    assert_eq!(
        &argv[..6],
        json!([
            "worker",
            "--enable=warning,performance,portability",
            "--error-exitcode=1",
            "--inline-suppr",
            "--language=c++",
            "--std=c++17"
        ])
        .as_array()
        .unwrap()
    );
    assert_eq!(argv.len(), 7);
    let list = argv[6]
        .as_str()
        .unwrap()
        .strip_prefix("--file-list=")
        .unwrap();
    assert!(Path::new(list).is_absolute());
    assert!(Path::new(list).starts_with(case.fixture.root.join("scratch")));
    assert!(!list.contains('{'));
    for stage in stages {
        assert!(stage["report"].is_null());
        assert_eq!(stage["sandbox"]["working_directory"], stage["snapshot"]);
    }
}
fn commit(case: &Case) {
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let tree_id = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(Some("HEAD"), &author, &author, "fixture", &tree, &[&parent])
        .unwrap();
}
fn assert_prelaunch_failure(case: &Case, before: &(Vec<u8>, Vec<u8>, String), needles: &[&str]) {
    let output = invoke(case, false, false);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        needles.iter().any(|needle| stderr.contains(needle)),
        "{stderr}"
    );
    assert!(
        !stderr.contains("ungranted"),
        "file validation must precede launch: {stderr}"
    );
    case.assert_preserved(before);
}

#[test]
fn staged_list_policy_selects_c_not_rust_or_dotfile_with_empty_path() {
    let case = case();
    case.fixture
        .stage("src/yes.CPP", "int yes(void) { return 1; }\n");
    case.fixture.stage(".c", "dotfile is not a suffix\n");
    let before = case.source_state();
    fs::write(
        case.fixture.root.join("repo/policies/main.toml"),
        "invalid dirty policy",
    )
    .unwrap();
    let report = value(invoke(&case, false, false), 0);
    prepared(&case, &report, json!(["src/yes.CPP"]));
    assert_eq!(report["check"]["file_input"], "cppcheck-list");
    assert_eq!(
        fs::read_to_string(case.fixture.root.join("repo/policies/main.toml")).unwrap(),
        "invalid dirty policy"
    );
    case.assert_preserved(&before);
}

#[test]
fn spaces_leading_dash_and_missing_dirty_worktree_use_staged_blobs_not_option_argv() {
    let case = case();
    case.fixture.stage("-leading.cpp", "int staged_one;\n");
    case.fixture.stage("dir/space name.C", "int staged_two;\n");
    let before = case.source_state();
    fs::write(
        case.fixture.root.join("repo/-leading.cpp"),
        "dirty replacement\n",
    )
    .unwrap();
    fs::remove_file(case.fixture.root.join("repo/dir/space name.C")).unwrap();
    let report = value(invoke(&case, false, false), 0);
    prepared(&case, &report, json!(["-leading.cpp", "dir/space name.C"]));
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let tree = repository
        .find_tree(Oid::from_str(report["candidate"]["tree_oid"].as_str().unwrap()).unwrap())
        .unwrap();
    for (path, bytes) in [
        ("-leading.cpp", b"int staged_one;\n".as_slice()),
        ("dir/space name.C", b"int staged_two;\n".as_slice()),
    ] {
        let entry = tree.get_path(Path::new(path)).unwrap();
        assert_eq!(repository.find_blob(entry.id()).unwrap().content(), bytes);
    }
    assert_eq!(
        fs::read(case.fixture.root.join("repo/-leading.cpp")).unwrap(),
        b"dirty replacement\n"
    );
    assert!(!case.fixture.root.join("repo/dir/space name.C").exists());
    case.assert_preserved(&before);
}

#[test]
fn renamed_c_to_rust_uses_current_suffix_and_skips_both_stages() {
    let mut case = case();
    let source = "int same(void) { return 4; }\n";
    case.fixture.stage("old.c", source);
    commit(&case);
    case.fixture.stage("new.rs", source);
    fs::remove_file(case.fixture.root.join("repo/old.c")).unwrap();
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let mut index = repository.index().unwrap();
    index.remove_path(Path::new("old.c")).unwrap();
    index.write().unwrap();
    case.grants["programs"] = json!({});
    let before = case.source_state();
    let report = value(invoke(&case, false, true), 0);
    assert!(report["candidate"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["path"] == "new.rs" && c["old_path"] == "old.c"));
    assert_eq!(report["files"], json!([]));
    assert_eq!(report["skipped"], true);
    assert_eq!(report["stages"], json!([]));
    assert!(report["result"].is_null());
    case.assert_preserved(&before);
}

#[test]
fn no_eligible_files_execute_skips_without_program_or_cgroup_and_keeps_batch_order() {
    let mut case = case();
    case.grants["programs"] = json!({});
    let before = case.source_state();
    let report = value(invoke(&case, false, true), 0);
    assert_eq!(report["skipped"], true);
    assert_eq!(report["stages"], json!([]));
    case.assert_preserved(&before);
    let report = value(invoke(&case, true, true), 0);
    assert_eq!(report["success"], true);
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["execution_scope"], "selected_regular_checks");
    assert_eq!(report["unexecuted"], json!([]));
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(
        checks
            .iter()
            .map(|c| c["check_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["zeta", "alpha"]
    );
    for row in checks {
        assert_eq!(row["skipped"], true);
        assert!(row["result"].is_null());
        assert_eq!(row["stages"], json!([]));
        assert_eq!(row["files"], json!([]));
    }
    assert_eq!(
        fs::read_dir(case.fixture.root.join("cgroup"))
            .unwrap()
            .count(),
        0
    );
    case.assert_preserved(&before);
}

#[test]
fn selected_backslash_collision_is_refused_before_stage_launch() {
    let mut case = case();
    case.fixture.stage("a\\b.cpp", "int backslash;\n");
    case.fixture.stage("a/b.cpp", "int directory;\n");
    case.grants["programs"] = json!({});
    let before = case.source_state();
    assert_prelaunch_failure(
        &case,
        &before,
        &[
            "backslash",
            "file_input",
            "file input",
            "cppcheck",
            "unsafe",
        ],
    );
    assert_eq!(
        fs::read(case.fixture.root.join("repo/a\\b.cpp")).unwrap(),
        b"int backslash;\n"
    );
    assert_eq!(
        fs::read(case.fixture.root.join("repo/a/b.cpp")).unwrap(),
        b"int directory;\n"
    );
}

#[test]
fn selected_symlink_refuses_even_safe_target_before_version_or_command() {
    let mut case = case();
    case.fixture.stage("target.txt", "int target;\n");
    let link = case.fixture.root.join("repo/link.cpp");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();
    case.fixture.add("link.cpp");
    case.grants["programs"] = json!({});
    let before = case.source_state();
    assert_prelaunch_failure(&case, &before, &["regular", "symlink", "mode"]);
    assert_eq!(fs::read_link(link).unwrap(), Path::new("target.txt"));
    assert_eq!(
        fs::read(case.fixture.root.join("repo/target.txt")).unwrap(),
        b"int target;\n"
    );
}

#[test]
#[ignore = "requires FORGE_TEST_CPPCHECK and a delegated FORGE_TEST_CGROUP_PARENT"]
fn delegated_cppcheck_consumes_each_selected_option_like_and_spaced_filename() {
    let mut case = case();
    let cppcheck = std::path::PathBuf::from(
        std::env::var_os("FORGE_TEST_CPPCHECK").expect("explicit cppcheck executable"),
    );
    assert!(cppcheck.is_absolute() && cppcheck.is_file());
    case.grants["programs"]["worker"] = json!(cppcheck.canonicalize().unwrap());
    case.delegated();
    let paths = [
        "--enable=all.cpp",
        "dir/space name.cpp",
        " leading'quote.cpp",
    ];
    for path in paths {
        case.fixture
            .stage(path, "int broken() { int *p = nullptr; return *p; }\n");
    }
    let before = case.source_state();
    let report = value(invoke(&case, false, true), 1);
    assert_eq!(report["result"]["status"], "failed");
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .starts_with("Cppcheck "));
    let diagnostics = report["result"]["stderr_tail"].as_str().unwrap();
    for path in paths {
        assert!(
            diagnostics.contains(path),
            "missing {path:?} from {diagnostics}"
        );
    }
    assert!(diagnostics.contains("nullPointer"));
    for stage in report["stages"].as_array().unwrap() {
        assert_eq!(stage["report"]["outcome"], "exited");
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    case.assert_preserved(&before);
}
