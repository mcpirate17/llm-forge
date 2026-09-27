#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for staged and live duplicate-audit orchestration.

#[path = "python_contracts/audit_fixture.rs"]
#[allow(dead_code)]
mod audit_fixture;
#[path = "python_contracts/duplicate_audit_fixture.rs"]
#[allow(dead_code)]
mod duplicate_audit_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use audit_fixture::{capture, captured, git, py_json, write, DictItemPatch};
use duplicate_audit_fixture::{
    audit, baseline_relative, call_baseline, completed, configure_jscpd, configure_pmd, dup_entry,
    files_under, jscpd_emulator, kwargs_root, pmd_emulator, repo, write_baseline, SENTINEL,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch};

fn py_error(py: Python<'_>, message: &str) -> PyErr {
    let instance = audit(py)
        .getattr("DuplicateAuditError")
        .unwrap()
        .call1((message,))
        .unwrap();
    PyErr::from_value(instance)
}

fn call_main(py: Python<'_>, argv: &[&str]) -> i32 {
    let _argv = AttrPatch::replace(
        module(py, "sys").as_any(),
        "argv",
        PyList::new(
            py,
            std::iter::once("run_duplicate_audit").chain(argv.iter().copied()),
        )
        .unwrap()
        .as_any(),
    );
    audit(py)
        .getattr("main")
        .unwrap()
        .call0()
        .unwrap()
        .extract()
        .unwrap()
}

fn run_with_root(py: Python<'_>, name: &str, root: &Path, check: bool, index: bool) -> i32 {
    let kwargs = kwargs_root(py, root);
    audit(py)
        .getattr(name)
        .unwrap()
        .call((check, index), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn resolve_audit_root_explicit_path_wins() {
    let (case, root) = repo();
    let explicit = case.mkdir("exported-candidate");
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("cwd", path(py, &root)).unwrap();
        let relative = path(py, Path::new("../exported-candidate"));
        let resolved = audit(py)
            .getattr("_resolve_audit_root")
            .unwrap()
            .call((relative,), Some(&kwargs))
            .unwrap();
        assert_eq!(
            resolved.extract::<PathBuf>().unwrap(),
            explicit.canonicalize().unwrap()
        );
    });
}

#[test]
fn resolve_audit_root_fails_closed_outside_git() {
    let (case, _) = repo();
    let outside = case.mkdir("not-a-worktree");
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("cwd", path(py, &outside)).unwrap();
        let error = audit(py)
            .getattr("_resolve_audit_root")
            .unwrap()
            .call((py.None(),), Some(&kwargs))
            .unwrap_err();
        let class = audit(py).getattr("DuplicateAuditError").unwrap();
        assert_error(py, error, &class, "pass --root explicitly");
    });
}

#[test]
fn main_passes_cwd_git_root_to_selected_tool() {
    let (case, root) = repo();
    let _cwd = case.chdir("repo");
    let seen = Arc::new(Mutex::new(None::<(bool, bool, bool, PathBuf, bool)>));
    let observed = seen.clone();
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<i32> {
            let kwargs = kw.unwrap();
            *observed.lock().unwrap() = Some((
                args.get_item(0)?.extract()?,
                args.get_item(1)?.extract()?,
                args.get_item(2)?.extract()?,
                kwargs.get_item("root")?.unwrap().extract()?,
                kwargs.get_item("changed_files")?.unwrap().is_none(),
            ));
            Ok(0)
        })
        .unwrap();
        let tools = audit(py)
            .getattr("TOOLS")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let _tool = DictItemPatch::replace(&tools, "jscpd", callback.as_any());
        let (stdout, _capture) = capture(py, "stdout");
        assert_eq!(call_main(py, &["--tool", "jscpd", "--check"]), 0);
        assert!(captured(&stdout).contains(&format!("audit-root: {}", root.display())));
        assert!(captured(&stdout).contains("mode: worktree"));
    });
    assert_eq!(
        *seen.lock().unwrap(),
        Some((true, false, false, root, true))
    );
}

#[test]
fn main_threads_changed_file_cli_flags_to_baseline_supported_tool() {
    let (case, _root) = repo();
    let _cwd = case.chdir("repo");
    let list = case.write("changed.txt", "c/three.py\n");
    let pmd_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = pmd_seen.clone();
    let nicad_seen = Arc::new(Mutex::new(false));
    let nicad = nicad_seen.clone();
    Python::attach(|py| {
        let pmd = PyCFunction::new_closure(py, None, None, move |_, kw| -> PyResult<i32> {
            let changed = kw.unwrap().get_item("changed_files")?.unwrap();
            let mut paths = changed
                .try_iter()?
                .map(|v| v.unwrap().extract::<String>().unwrap())
                .collect::<Vec<_>>();
            paths.sort();
            *seen.lock().unwrap() = paths;
            Ok(0)
        })
        .unwrap();
        let ni = PyCFunction::new_closure(py, None, None, move |_, kw| -> PyResult<i32> {
            assert!(kw.unwrap().get_item("changed_files")?.is_none());
            *nicad.lock().unwrap() = true;
            Ok(0)
        })
        .unwrap();
        let tools = audit(py)
            .getattr("TOOLS")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let _pmd = DictItemPatch::replace(&tools, "pmd-python", pmd.as_any());
        let _nicad = DictItemPatch::replace(&tools, "nicad-python", ni.as_any());
        assert_eq!(
            call_main(
                py,
                &[
                    "--tool",
                    "pmd-python",
                    "--tool",
                    "nicad-python",
                    "--check",
                    "--changed-file",
                    "a/one.py",
                    "--changed-files-from",
                    list.to_str().unwrap()
                ]
            ),
            0
        );
    });
    assert_eq!(*pmd_seen.lock().unwrap(), vec!["a/one.py", "c/three.py"]);
    assert!(*nicad_seen.lock().unwrap());
}

#[test]
fn jscpd_live_scan_uses_git_visible_sources() {
    let (_case, root) = repo();
    Python::attach(|py| {
        configure_jscpd(py, &root, &[]);
        write(&root, "src/.gitignore", "reports/\n");
        git(&root, &["add", "src/.gitignore"]);
        for name in [
            "src/reports/ignored_a.py",
            "src/reports/ignored_b.py",
            "src/visible_a.py",
            "native/visible_b.py",
        ] {
            write(&root, name, &format!("{SENTINEL}\n"));
        }
        let seen = Arc::new(Mutex::new(HashSet::<String>::new()));
        let observed = seen.clone();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let cwd: PathBuf = kw.unwrap().get_item("cwd")?.unwrap().extract()?;
                let paths: Vec<String> = args.get_item(0)?.extract()?;
                for source in paths {
                    for file in files_under(&cwd.join(source)) {
                        observed.lock().unwrap().insert(
                            file.strip_prefix(&cwd)
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_owned(),
                        );
                    }
                }
                let entry = dup_entry(py, "src/visible_a.py", "native/visible_b.py", SENTINEL);
                Ok(py_json(py, &json!([entry])).unbind())
            })
            .unwrap();
        let _collect = AttrPatch::replace(
            audit(py).as_any(),
            "_jscpd_collect_duplicates",
            callback.as_any(),
        );
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, false), 1);
        let found = seen.lock().unwrap();
        assert!(found.contains("src/visible_a.py") && found.contains("native/visible_b.py"));
        assert!(
            !found.contains("src/reports/ignored_a.py")
                && !found.contains("src/reports/ignored_b.py")
        );
    });
}

#[test]
fn materialized_sources_are_exact_index_blobs() {
    let (_case, root) = repo();
    write(&root, "src/tracked.py", "INDEX_VERSION = 1\n");
    fs::write(
        root.join("src/checkpoint.pt"),
        b"tracked artifact must not be copied",
    )
    .unwrap();
    git(&root, &["add", "src/tracked.py", "src/checkpoint.pt"]);
    write(&root, "src/tracked.py", "WORKTREE_VERSION = 2\n");
    write(&root, "src/untracked.py", "UNTRACKED_VERSION = 3\n");
    Python::attach(|py| {
        for staged in [false, true] {
            if staged {
                git(&root, &["add", "src/tracked.py", "src/untracked.py"]);
            }
            let builtins = module(py, "builtins");
            let suffixes = builtins
                .getattr("frozenset")
                .unwrap()
                .call1((vec![".py"],))
                .unwrap();
            let kwargs = kwargs_root(py, &root);
            let manager = audit(py)
                .getattr("materialized_index_sources")
                .unwrap()
                .call((("src",), suffixes), Some(&kwargs))
                .unwrap();
            let snapshot: PathBuf = manager
                .call_method0("__enter__")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                fs::read_to_string(snapshot.join("src/tracked.py")).unwrap(),
                if staged {
                    "WORKTREE_VERSION = 2\n"
                } else {
                    "INDEX_VERSION = 1\n"
                }
            );
            assert_eq!(snapshot.join("src/untracked.py").exists(), staged);
            assert!(!snapshot.join("src/checkpoint.pt").exists());
            manager
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
        }
    });
}

#[test]
fn vulture_check_ignores_untracked_but_fails_for_index_violation() {
    let (_case, root) = repo();
    write(
        &root,
        "src/definition.py",
        "def shared_symbol():\n    return 1\n",
    );
    write(
        &root,
        "src/consumer.py",
        "from .definition import shared_symbol\nRESULT = shared_symbol()\n",
    );
    git(&root, &["add", "src/definition.py", "src/consumer.py"]);
    write(
        &root,
        "src/untracked_violation.py",
        "UNUSED_INDEX_SENTINEL = object()\n",
    );
    Python::attach(|py| {
        let detector = PyCFunction::new_closure(py, None, None, |args, kw| -> PyResult<i32> {
            let command: Vec<String> = args.get_item(0)?.extract()?;
            let cwd: PathBuf = kw.unwrap().get_item("cwd")?.unwrap().extract()?;
            let mut texts = Vec::new();
            for arg in command {
                let path = PathBuf::from(arg);
                if path.to_string_lossy().contains("llm-index-sources-") && path.is_dir() {
                    texts.extend(
                        files_under(&path)
                            .iter()
                            .map(|p| fs::read_to_string(p).unwrap()),
                    );
                }
            }
            if cwd
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("llm-index-sources-"))
            {
                texts.extend(
                    files_under(&cwd)
                        .iter()
                        .map(|p| fs::read_to_string(p).unwrap()),
                );
            }
            assert!(texts.iter().any(|t| t.contains("def shared_symbol")));
            assert!(texts.iter().any(|t| t.contains("shared_symbol()")));
            Ok(i32::from(
                texts.iter().any(|t| t.contains("UNUSED_INDEX_SENTINEL")),
            ))
        })
        .unwrap();
        let _run = AttrPatch::replace(audit(py).as_any(), "run", detector.as_any());
        assert_eq!(run_with_root(py, "run_vulture", &root, true, true), 0);
        git(&root, &["add", "src/untracked_violation.py"]);
        assert_eq!(run_with_root(py, "run_vulture", &root, true, true), 1);
    });
}

#[test]
fn jscpd_check_ignores_untracked_but_fails_for_index_duplicates() {
    let (_case, root) = repo();
    Python::attach(|py| {
        configure_jscpd(py, &root, &[]);
        write(&root, "src/base.py", "VALUE = 1\n");
        git(&root, &["add", "src/base.py"]);
        write(&root, "src/untracked_a.py", &format!("{SENTINEL}\n"));
        write(&root, "native/untracked_b.py", &format!("{SENTINEL}\n"));
        let _report = jscpd_emulator(py, false);
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, true), 0);
        git(
            &root,
            &["add", "src/untracked_a.py", "native/untracked_b.py"],
        );
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, true), 1);
    });
}

#[test]
fn jscpd_snapshot_preserves_repository_relative_ignores() {
    let (_case, root) = repo();
    Python::attach(|py| {
        configure_jscpd(py, &root, &["src/tests/**"]);
        write(&root, "src/tests/ignored.py", &format!("{SENTINEL}\n"));
        write(&root, "src/tests/ignored_peer.py", &format!("{SENTINEL}\n"));
        git(
            &root,
            &["add", "src/tests/ignored.py", "src/tests/ignored_peer.py"],
        );
        write(&root, "package.json", "{\"jscpd\":{\"ignore\":[]}}\n");
        let _report = jscpd_emulator(py, true);
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, true), 0);
    });
}

fn report_failure(py: Python<'_>, analyzer: &str, failure: &str) -> AttrPatch {
    let expected = analyzer.to_owned();
    let mode = failure.to_owned();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let command: Vec<String> = args.get_item(0)?.extract()?;
            let tool: String = kw.unwrap().get_item("tool_name")?.unwrap().extract()?;
            assert_eq!(tool, expected);
            if mode == "nonzero" {
                return Err(py_error(py, &format!("{tool} exited 9; report rejected")));
            }
            if mode == "malformed" {
                if tool == "jscpd" {
                    let out = PathBuf::from(
                        &command[command.iter().position(|v| v == "--output").unwrap() + 1],
                    );
                    fs::create_dir_all(&out).unwrap();
                    fs::write(out.join("jscpd-report.json"), "{").unwrap();
                } else {
                    let report = PathBuf::from(
                        &command[command.iter().position(|v| v == "--report-file").unwrap() + 1],
                    );
                    fs::write(report, "<pmd-cpd>").unwrap();
                }
            }
            Ok(completed(py, &args.get_item(0)?, 0, ""))
        })
        .unwrap();
    AttrPatch::replace(audit(py).as_any(), "_run_report_command", callback.as_any())
}

fn assert_report_failure(kind: &str, mode: &str) {
    let (_case, root) = repo();
    Python::attach(|py| {
        if kind == "jscpd" {
            configure_jscpd(py, &root, &[]);
        } else {
            configure_pmd(&root);
            let baseline = baseline_relative(py, "PMD_CPD_BASELINE_RELATIVE");
            write_baseline(&root.join(baseline), &[]);
        }
        write(&root, "src/source.py", "VALUE = 1\n");
        let _report = report_failure(py, kind, mode);
        let function = if kind == "jscpd" {
            "run_jscpd"
        } else {
            "run_pmd_python"
        };
        let kwargs = kwargs_root(py, &root);
        let result: i32 = audit(py)
            .getattr(function)
            .unwrap()
            .call((true,), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        let expected: i32 = audit(py)
            .getattr("AUDIT_ERROR_EXIT_CODE")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, expected);
    });
}

#[test]
fn jscpd_report_nonzero_is_blocking() {
    assert_report_failure("jscpd", "nonzero");
}
#[test]
fn jscpd_report_missing_is_blocking() {
    assert_report_failure("jscpd", "missing");
}
#[test]
fn jscpd_report_malformed_is_blocking() {
    assert_report_failure("jscpd", "malformed");
}
#[test]
fn pmd_report_nonzero_is_blocking() {
    assert_report_failure("pmd-cpd", "nonzero");
}
#[test]
fn pmd_report_missing_is_blocking() {
    assert_report_failure("pmd-cpd", "missing");
}
#[test]
fn pmd_report_malformed_is_blocking() {
    assert_report_failure("pmd-cpd", "malformed");
}

fn report_command_rejects(kind: &str) {
    let (case, _) = repo();
    Python::attach(|py| {
        let fake = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(completed(
                args.py(),
                &args.get_item(0)?,
                9,
                "analyzer crashed",
            ))
        })
        .unwrap();
        let _run = AttrPatch::replace(module(py, "subprocess").as_any(), "run", fake.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("cwd", path(py, case.root())).unwrap();
        kwargs.set_item("tool_name", kind).unwrap();
        let error = audit(py)
            .getattr("_run_report_command")
            .unwrap()
            .call((vec!["analyzer"],), Some(&kwargs))
            .unwrap_err();
        let class = audit(py).getattr("DuplicateAuditError").unwrap();
        assert_error(
            py,
            error,
            &class,
            &format!("{kind} exited 9; report rejected: analyzer crashed"),
        );
    });
}
#[test]
fn jscpd_report_command_rejects_nonzero_exit() {
    report_command_rejects("jscpd");
}
#[test]
fn pmd_report_command_rejects_nonzero_exit() {
    report_command_rejects("pmd-cpd");
}

fn invalid_baseline(payload: Value) {
    let (case, _) = repo();
    let baseline = case.write("baseline.json", &payload.to_string());
    Python::attach(|py| {
        let code = call_baseline(py, &baseline, &[], None);
        let error: i32 = audit(py)
            .getattr("AUDIT_ERROR_EXIT_CODE")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, error);
    });
}
#[test]
fn baseline_count_mismatch_is_rejected() {
    invalid_baseline(json!({"_comment":"test","count":1,"entries":{}}));
}
#[test]
fn baseline_invalid_key_is_rejected() {
    invalid_baseline(json!({"_comment":"test","count":1,
        "entries":{"not-a-clone-key":{"files":["a.py","b.py"],"lines":10}}}));
}

fn baseline_case() -> (support::Case, PathBuf) {
    let (case, _) = repo();
    let baseline = case.root().join("baseline.json");
    write_baseline(&baseline, &[]);
    (case, baseline)
}

#[test]
fn baseline_without_changed_files_blocks_every_new_pair() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let left = dup_entry(py, "a/one.py", "b/two.py", "LEFT_SENTINEL = 1");
        let right = dup_entry(py, "c/three.py", "d/four.py", "RIGHT_SENTINEL = 1");
        assert_eq!(call_baseline(py, &baseline, &[left, right], None), 1);
    });
}

#[test]
fn baseline_caused_via_left_side_blocks() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let entry = dup_entry(py, "a/one.py", "b/two.py", "LEFT_SENTINEL = 1");
        assert_eq!(
            call_baseline(py, &baseline, &[entry], Some(&["a/one.py"])),
            1
        );
    });
}

#[test]
fn baseline_caused_via_right_side_blocks() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let entry = dup_entry(py, "a/one.py", "b/two.py", "RIGHT_SENTINEL = 1");
        assert_eq!(
            call_baseline(py, &baseline, &[entry], Some(&["b/two.py"])),
            1
        );
    });
}

#[test]
fn baseline_inherited_via_neither_side_does_not_block() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let entry = dup_entry(py, "a/one.py", "b/two.py", "NEITHER_SENTINEL = 1");
        assert_eq!(
            call_baseline(py, &baseline, &[entry], Some(&["z/unrelated.py"])),
            0
        );
    });
}

#[test]
fn baseline_no_new_findings_exits_zero_either_way() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let entry = dup_entry(py, "a/one.py", "b/two.py", "ALREADY_KNOWN = 1");
        write_baseline(&baseline, std::slice::from_ref(&entry));
        assert_eq!(
            call_baseline(py, &baseline, std::slice::from_ref(&entry), None),
            0
        );
        assert_eq!(
            call_baseline(py, &baseline, &[entry], Some(&["a/one.py"])),
            0
        );
    });
}

#[test]
fn changed_baseline_only_file_is_not_reported_as_caused() {
    let (_case, baseline) = baseline_case();
    Python::attach(|py| {
        let known = dup_entry(py, "a/one.py", "b/two.py", "ALREADY_KNOWN = 1");
        write_baseline(&baseline, std::slice::from_ref(&known));
        let fresh = dup_entry(py, "c/three.py", "d/four.py", "BRAND_NEW = 1");
        assert_eq!(
            call_baseline(py, &baseline, &[known, fresh], Some(&["a/one.py"])),
            0
        );
    });
}

#[test]
fn jscpd_index_check_reads_staged_baseline() {
    let (_case, root) = repo();
    Python::attach(|py| {
        configure_jscpd(py, &root, &[]);
        let fragment = SENTINEL;
        write(&root, "src/first.py", &format!("{fragment}\n"));
        write(&root, "native/second.py", &format!("{fragment}\n"));
        git(&root, &["add", "src/first.py", "native/second.py"]);
        let entry = dup_entry(py, "src/first.py", "native/second.py", fragment);
        let baseline = baseline_relative(py, "JSCPD_BASELINE_RELATIVE");
        write_baseline(&root.join(&baseline), &[entry]);
        let _report = jscpd_emulator(py, false);
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, true), 1);
        git(&root, &["add", &baseline]);
        assert_eq!(run_with_root(py, "run_jscpd", &root, true, true), 0);
    });
}

#[test]
fn resolve_pmd_executable_prefers_an_explicit_override() {
    let (_case, root) = repo();
    configure_pmd(&root);
    Python::attach(|py| {
        let result = audit(py)
            .getattr("_resolve_pmd_executable")
            .unwrap()
            .call1((path(py, &root), "/custom/pmd"))
            .unwrap();
        assert_eq!(text(&result), "/custom/pmd");
    });
}

#[test]
fn resolve_pmd_executable_prefers_a_project_local_binary() {
    let (_case, root) = repo();
    configure_pmd(&root);
    Python::attach(|py| {
        let which = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<&str> {
            Ok("/usr/bin/unrelated-pmd")
        })
        .unwrap();
        let _which = AttrPatch::replace(
            audit(py).getattr("shutil").unwrap().as_any(),
            "which",
            which.as_any(),
        );
        let result = audit(py)
            .getattr("_resolve_pmd_executable")
            .unwrap()
            .call1((path(py, &root),))
            .unwrap();
        assert_eq!(
            text(&result),
            root.join("node_modules/.bin/pmd")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
    });
}

#[test]
fn resolve_pmd_executable_falls_back_to_path() {
    let (_case, root) = repo();
    Python::attach(|py| {
        let which = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<&str> {
            Ok("/usr/local/bin/pmd")
        })
        .unwrap();
        let _which = AttrPatch::replace(
            audit(py).getattr("shutil").unwrap().as_any(),
            "which",
            which.as_any(),
        );
        let result = audit(py)
            .getattr("_resolve_pmd_executable")
            .unwrap()
            .call1((path(py, &root),))
            .unwrap();
        assert_eq!(text(&result), "/usr/local/bin/pmd");
    });
}

#[test]
fn resolve_pmd_executable_raises_when_nothing_resolves() {
    let (_case, root) = repo();
    Python::attach(|py| {
        let which = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(args.py().None())
        })
        .unwrap();
        let _which = AttrPatch::replace(
            audit(py).getattr("shutil").unwrap().as_any(),
            "which",
            which.as_any(),
        );
        let error = audit(py)
            .getattr("_resolve_pmd_executable")
            .unwrap()
            .call1((path(py, &root),))
            .unwrap_err();
        let class = audit(py).getattr("DuplicateAuditError").unwrap();
        assert_error(py, error, &class, "pmd executable is unavailable");
    });
}

#[test]
fn pmd_index_check_reads_staged_baseline() {
    let (_case, root) = repo();
    configure_pmd(&root);
    Python::attach(|py| {
        let fragment = SENTINEL;
        write(&root, "src/first.py", &format!("{fragment}\n"));
        write(&root, "native/second.py", &format!("{fragment}\n"));
        let baseline = baseline_relative(py, "PMD_CPD_BASELINE_RELATIVE");
        write_baseline(&root.join(&baseline), &[]);
        git(
            &root,
            &["add", "src/first.py", "native/second.py", &baseline],
        );
        let entry = dup_entry(py, "src/first.py", "native/second.py", fragment);
        write_baseline(&root.join(&baseline), &[entry]);
        let _report = pmd_emulator(py);
        assert_eq!(run_with_root(py, "run_pmd_python", &root, true, true), 1);
        git(&root, &["add", &baseline]);
        assert_eq!(run_with_root(py, "run_pmd_python", &root, true, true), 0);
    });
}
