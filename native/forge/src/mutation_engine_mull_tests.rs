use super::*;
use crate::mutation_run_test_support::campaign;

const SOURCE: &str = "/w/src/norm.cpp";
const TEXT: &str = "void f(int n) {\n  for (int i = 0; i < n; ++i) {\n    g(i * 2);\n  }\n}\n";

fn mutant(id: &str, status: &str, line: u64, column: u64, end_column: u64) -> Value {
    json!({
        "id": id,
        "mutatorName": "cxx_lt_to_le",
        "replacement": "<=",
        "status": status,
        "location": {
            "start": {"line": line, "column": column},
            "end": {"line": line, "column": end_column},
        },
    })
}

fn report(path: &str, mutants: Vec<Value>) -> Value {
    json!({
        "config": {"mullVersion": "0.34.0"},
        "files": {path: {"source": TEXT, "mutants": mutants}},
    })
}

fn location(start: (u64, u64), end: (u64, u64)) -> Value {
    json!({
        "start": {"line": start.0, "column": start.1},
        "end": {"line": end.0, "column": end.1},
    })
}

fn merged_rows(mutants: Vec<Value>) -> Result<Vec<Value>, String> {
    rows(&merge(&[report(SOURCE, mutants)]), Path::new("/w"))
}

fn mull_campaign(dir: &Path, options: Value) -> Campaign {
    let mut subject = campaign(
        dir,
        json!({
            "language": "cpp",
            "mutation_engine": "mull",
            "generator": {
                "source": ["src/**/*.cpp"],
                "exclude": ["cxx_add_to_sub"],
                "run_timeout_seconds": 60,
                "options": options,
            },
        }),
    );
    subject.mutant_timeout_seconds = Some(3);
    subject
}

fn argv(subject: &Campaign) -> Vec<String> {
    engine_argv(
        subject,
        &RunnerCall {
            binary: "/usr/bin/mull-runner-18",
            executable: Path::new("/b/suite"),
            profdata: Path::new("/b/suite.profdata"),
            report_dir: Path::new("/b/reports"),
            report_name: "suite",
        },
    )
    .unwrap()
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let position = argv.iter().position(|arg| arg == flag).unwrap();
    &argv[position + 1]
}

fn result(returncode: Option<i32>, timed_out: bool) -> CommandResult {
    CommandResult {
        returncode,
        timed_out,
        duration_seconds: 0.0,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
    }
}

#[test]
fn every_status_is_mapped_and_none_is_guessed() {
    for (status, expected) in [
        ("Killed", KILLED),
        ("Survived", SURVIVED),
        ("NoCoverage", NO_COVERAGE),
        ("Timeout", TIMED_OUT),
        ("CompileError", UNVIABLE),
        ("Ignored", UNVIABLE),
        ("RuntimeError", ERROR),
    ] {
        let rows = merged_rows(vec![mutant("m", status, 2, 21, 22)]).unwrap();
        assert_eq!(rows[0]["outcome"], expected, "{status}");
    }
    let error = merged_rows(vec![mutant("m", "Pending", 2, 21, 22)]).unwrap_err();
    assert_eq!(error, "Mull reported unknown status 'Pending'");
}

#[test]
fn every_field_a_row_carries_is_pinned() {
    let rows = merged_rows(vec![mutant("m", "Killed", 2, 21, 22)]).unwrap();
    let row = rows[0].as_object().unwrap();
    assert_eq!(
        row.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "id",
            "outcome",
            "path",
            "line",
            "operator",
            "original_text",
            "mutated_text"
        ]
    );
    assert_eq!(row["path"], "src/norm.cpp");
    assert_eq!(row["line"], 2);
    assert_eq!(row["operator"], "cxx_lt_to_le");
    assert_eq!(row["original_text"], "<");
    assert_eq!(row["mutated_text"], "<=");
}

#[test]
fn a_mutant_is_named_independently_of_its_line_and_repeats_stay_separate() {
    let named = |source: &str, line: u64| {
        let report = json!({"files": {SOURCE: {
            "source": source,
            "mutants": [mutant("m", "Killed", line, 3, 4)],
        }}});
        rows(&merge(&[report]), Path::new("/w")).unwrap()[0]["id"].clone()
    };
    assert_eq!(named("a < b\n", 1), named("\n\n\na < b\n", 4));
    let repeats = merged_rows(vec![
        mutant("a", "Killed", 2, 21, 22),
        mutant("b", "Survived", 2, 21, 22),
    ])
    .unwrap();
    assert_eq!(repeats.len(), 2);
    assert_ne!(repeats[0]["id"], repeats[1]["id"]);
}

#[test]
fn a_mutant_outside_the_worktree_stops_the_run() {
    let merged = merge(&[report(
        "/elsewhere/norm.cpp",
        vec![mutant("m", "Killed", 2, 21, 22)],
    )]);
    assert_eq!(
        rows(&merged, Path::new("/w")).unwrap_err(),
        "Mull reported a mutant outside the worktree: /elsewhere/norm.cpp"
    );
}

#[test]
fn a_mutant_with_no_location_is_refused() {
    let mut unplaced = mutant("m", "Killed", 2, 21, 22);
    unplaced["location"] = json!({});
    assert_eq!(
        merged_rows(vec![unplaced]).unwrap_err(),
        "Mull reported a mutant with no start.line"
    );
}

#[test]
fn the_original_text_is_sliced_out_of_the_reported_source() {
    assert_eq!(slice(TEXT, &location((2, 21), (2, 22))).unwrap(), "<");
    assert_eq!(slice(TEXT, &location((1, 1), (1, 5))).unwrap(), "void");
    assert_eq!(
        slice(TEXT, &location((1, 15), (3, 7))).unwrap(),
        "{\n  for (int i = 0; i < n; ++i) {\n    g("
    );
    assert_eq!(slice(TEXT, &location((99, 1), (99, 2))).unwrap(), "");
    assert_eq!(slice(TEXT, &location((5, 1), (99, 2))).unwrap(), "}");
    assert_eq!(slice(TEXT, &location((1, 90), (1, 99))).unwrap(), "");
}

#[test]
fn lines_are_split_where_python_splits_them() {
    let lines: Vec<String> = split_lines("a\r\nb\u{c}c\n\nd")
        .iter()
        .map(|line| line.iter().collect())
        .collect();
    assert_eq!(lines, ["a", "b", "c", "", "d"]);
    assert!(split_lines("").is_empty());
}

#[test]
fn a_kill_by_any_executable_outranks_every_other_verdict() {
    let first = report(SOURCE, vec![mutant("m", "Survived", 2, 21, 22)]);
    let second = report(SOURCE, vec![mutant("m", "Killed", 2, 21, 22)]);
    for reports in [[first.clone(), second.clone()], [second, first]] {
        let merged = merge(&reports);
        let (_, mutants) = &merged[SOURCE];
        assert_eq!(mutants.len(), 1);
        assert_eq!(mutants[0]["status"], "Killed");
    }
}

#[test]
fn merging_keeps_the_better_verdict_in_the_pinned_rank() {
    for (better_status, worse_status) in [
        ("Timeout", "Survived"),
        ("Survived", "CompileError"),
        ("CompileError", "Ignored"),
        ("Ignored", "NoCoverage"),
        ("NoCoverage", "Unheard"),
    ] {
        let merged = merge(&[
            report(SOURCE, vec![mutant("m", worse_status, 2, 21, 22)]),
            report(SOURCE, vec![mutant("m", better_status, 2, 21, 22)]),
        ]);
        assert_eq!(merged[SOURCE].1[0]["status"], better_status);
    }
}

#[test]
fn the_manifests_globs_scope_a_corpus_the_tool_cannot_scope() {
    let patterns = ["src/*.cpp".to_string(), "lib/**/x?.c".to_string()];
    assert!(in_scope("src/a.cpp", &patterns).unwrap());
    assert!(!in_scope("src/deep/a.cpp", &patterns).unwrap());
    assert!(in_scope("lib/a/b/x1.c", &patterns).unwrap());
    assert!(!in_scope("lib/a/x/1.c", &patterns).unwrap());
    assert!(!in_scope("other/a.cpp", &patterns).unwrap());
    assert!(!in_scope("src/a.cppx", &patterns).unwrap());
}

#[test]
fn the_summary_counts_the_scoped_corpus_and_only_its_errors() {
    let every = [
        json!({"path": "src/a.cpp", "outcome": ERROR}),
        json!({"path": "src/a.cpp", "outcome": KILLED}),
        json!({"path": "vendor/b.cpp", "outcome": ERROR}),
    ];
    assert_eq!(
        summary(&every, &every[..2]),
        json!({
            "mutants_reported": 3,
            "mutants_in_scope": 2,
            "mutants_generated": 2,
            "errors": 1,
            "files_mutated": ["src/a.cpp", "vendor/b.cpp"],
        })
    );
}

#[test]
fn rows_outside_the_declared_source_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let subject = mull_campaign(dir.path(), json!({}));
    let every = [
        json!({"path": "src/deep/a.cpp"}),
        json!({"path": "vendor/b.cpp"}),
    ];
    assert_eq!(scoped_rows(&subject, &every).unwrap(), &every[..1]);
}

#[test]
fn the_timeout_is_milliseconds_and_pinned_as_its_own_floor() {
    let dir = tempfile::tempdir().unwrap();
    let argv = argv(&mull_campaign(dir.path(), json!({})));
    assert_eq!(after(&argv, "--timeout"), "3000");
    assert_eq!(after(&argv, "--minimum-timeout"), "3000");
}

#[test]
fn the_runner_uses_real_coverage_and_owns_no_survivor_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let argv = argv(&mull_campaign(dir.path(), json!({})));
    assert_eq!(argv[..2], ["/usr/bin/mull-runner-18", "/b/suite"]);
    assert_eq!(after(&argv, "--coverage-info"), "/b/suite.profdata");
    assert_eq!(after(&argv, "--reporters"), "Elements");
    assert_eq!(after(&argv, "--report-dir"), "/b/reports");
    assert_eq!(after(&argv, "--report-name"), "suite");
    assert_eq!(after(&argv, "--ignore-mutators"), "cxx_add_to_sub");
    assert!(argv.iter().any(|arg| arg == "--allow-surviving"));
}

#[test]
fn the_build_carries_the_pass_plugin_and_both_instrumentations() {
    let dir = tempfile::tempdir().unwrap();
    let subject = mull_campaign(
        dir.path(),
        json!({"cmake_source_dir": "suite", "cmake_args": ["-DX=1"]}),
    );
    let argv = configure_argv(&subject, Path::new("/w"), Path::new("/w/b"), "18").unwrap();
    assert_eq!(after(&argv, "-S"), "/w/suite");
    assert_eq!(after(&argv, "-B"), "/w/b");
    assert_eq!(after(&argv, "-G"), "Ninja");
    assert!(argv.iter().any(|arg| arg == "-DCMAKE_C_COMPILER=clang-18"));
    assert!(argv
        .iter()
        .any(|arg| arg == "-DCMAKE_CXX_COMPILER=clang++-18"));
    for language in ["C", "CXX"] {
        let flags = format!(
            "-DCMAKE_{language}_FLAGS=-fpass-plugin=/usr/lib/mull-ir-frontend-18 -g \
             -grecord-command-line -fprofile-instr-generate -fcoverage-mapping"
        );
        assert!(argv.contains(&flags), "{argv:?}");
    }
    assert_eq!(argv.last().unwrap(), "-DX=1");
}

#[test]
fn a_campaign_missing_its_toolchain_pins_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bare = mull_campaign(dir.path(), json!({}));
    assert!(llvm_version(&bare).unwrap_err().contains("llvm_version"));
    assert!(executables(&bare).unwrap_err().contains("executables"));
    let error = configure_argv(&bare, Path::new("/w"), Path::new("/b"), "18").unwrap_err();
    assert!(error.contains("cmake_source_dir"), "{error}");
    let empty = mull_campaign(dir.path(), json!({"executables": []}));
    assert!(executables(&empty).is_err());
    let named = mull_campaign(dir.path(), json!({"executables": ["a", "sub/b"]}));
    assert_eq!(executables(&named).unwrap(), ["a", "sub/b"]);
}

#[test]
fn the_engine_result_is_the_first_failure_across_executables() {
    assert_eq!(
        engine_result(&[result(Some(0), false), result(Some(0), false)]),
        json!({"returncode": 0, "timed_out": false, "executables": 2})
    );
    assert_eq!(
        engine_result(&[
            result(Some(0), false),
            result(Some(7), false),
            result(Some(9), false)
        ]),
        json!({"returncode": 7, "timed_out": false, "executables": 3})
    );
    assert_eq!(
        engine_result(&[result(None, true)]),
        json!({"returncode": null, "timed_out": true, "executables": 1})
    );
}

#[test]
fn an_absent_or_malformed_report_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("suite.json");
    let failed = CommandResult {
        stderr_tail: "boom".into(),
        ..result(Some(1), false)
    };
    let error = report_at(&path, &failed).unwrap_err();
    assert!(error.contains("wrote no Elements report at"), "{error}");
    assert!(error.contains("(rc=1): boom"), "{error}");
    fs::write(&path, "not json").unwrap();
    assert!(report_at(&path, &failed)
        .unwrap_err()
        .contains("is not JSON"));
    fs::write(&path, "{}").unwrap();
    assert_eq!(report_at(&path, &failed).unwrap(), json!({}));
}

#[test]
fn a_report_is_named_after_the_executable_alone() {
    assert_eq!(base_name("tests/unit/suite"), "suite");
    assert_eq!(base_name("suite"), "suite");
}

fn finished(returncode: Option<i32>) -> CommandResult {
    CommandResult {
        returncode,
        timed_out: false,
        duration_seconds: 0.0,
        stdout_tail: String::new(),
        stderr_tail: "plugin mismatch".to_string(),
    }
}

#[test]
fn a_runner_that_exited_cleanly_is_a_measurement() {
    assert_eq!(require_runner_success(&finished(Some(0)), "suite"), Ok(()));
}

#[test]
fn a_report_from_a_failed_runner_is_refused() {
    let error = require_runner_success(&finished(Some(1)), "suite").unwrap_err();
    assert_eq!(error, "mull-runner failed on suite (rc=1): plugin mismatch");
    let error = require_runner_success(&finished(None), "suite").unwrap_err();
    assert_eq!(
        error,
        "mull-runner failed on suite (rc=None): plugin mismatch"
    );
}

#[test]
fn mull_environment_exports_only_the_selected_optional_guest() {
    let dir = tempfile::tempdir().unwrap();
    crate::mutation_run_test_support::write(dir.path(), "src/norm.cpp", "int f() { return 1; }\n");
    for interpreter in [None, Some("/selected/python")] {
        crate::mutation_run_test_support::with_run(
            dir.path(),
            interpreter,
            json!({"mutation_engine":"mull", "language":"cpp",
                "generator":{"source":["src/norm.cpp"],"run_timeout_seconds":60},
                "source_sha256":{"src/norm.cpp":"0".repeat(64)},
                "environment":{"CONDUCTOR_SNAPSHOT_PYTHON":"/stale/campaign","KEEP":"yes"}}),
            |run| {
                let bound = environment(run, dir.path()).unwrap();
                assert_eq!(
                    bound.get("CONDUCTOR_SNAPSHOT_PYTHON").map(String::as_str),
                    interpreter
                );
                assert_eq!(bound["KEEP"], "yes");
                assert!(Path::new(&bound["MULL_CONFIG"]).is_file());
            },
        );
    }
}
