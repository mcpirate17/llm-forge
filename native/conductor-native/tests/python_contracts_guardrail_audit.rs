#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped host guardrail audit.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};

const GIT_SELECTORS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG",
    "GIT_TEMPLATE_DIR",
];

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in GIT_SELECTORS {
        case.remove_env(name);
    }
    for name in ["PYLINT_HOME", "CONDUCTOR_VULTURE_WHITELIST"] {
        case.remove_env(name);
    }
    let pylint_config = case.write("pylint-default.rc", "[MASTER]\n");
    case.set_env("PYLINTRC", pylint_config.to_str().unwrap());
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case.set_env("CUDA_VISIBLE_DEVICES", "");
    case
}

fn audit<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.guardrail_audit")
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn patch_root(py: Python<'_>, root: &Path) -> AttrPatch {
    AttrPatch::replace(audit(py).as_any(), "ROOT", path(py, root).as_any())
}

fn check_args(args: &Bound<'_, pyo3::types::PyTuple>, count: usize) -> PyResult<()> {
    if args.len() == count {
        Ok(())
    } else {
        Err(PyAssertionError::new_err(format!(
            "expected {count} arguments"
        )))
    }
}

fn string_io<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = string_io(py);
    let guard = AttrPatch::replace(module(py, "sys").as_any(), stream, buffer.as_any());
    (buffer, guard)
}

fn issue<'py>(py: Python<'py>, kind: &str, severity: &str) -> Bound<'py, PyAny> {
    audit(py)
        .getattr("Issue")
        .unwrap()
        .call1((
            kind,
            severity,
            "research/example.py",
            "example",
            "Function complexity is high.",
            "Extract a helper.",
            PyDict::new(py),
        ))
        .unwrap()
}

fn stub_external(py: Python<'_>, root: &Path, results: &[(i32, &str)]) -> Vec<AttrPatch> {
    let queue = Arc::new(Mutex::new(
        results
            .iter()
            .map(|(code, output)| (*code, (*output).to_owned()))
            .collect::<VecDeque<_>>(),
    ));
    let file = root.join("example.py");
    let iter_files =
        PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
            let py = args.py();
            Ok(PyList::new(py, [path(py, &file)])?.into_any().unbind())
        })
        .unwrap();
    let structural =
        PyCFunction::new_closure(py, None, None, move |_, _| -> PyResult<(Vec<i32>, i32)> {
            Ok((Vec::new(), 1))
        })
        .unwrap();
    let resolve =
        PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Vec<String>> {
            let mut command = Vec::new();
            for arg in args.iter() {
                command.push(arg.extract::<String>()?);
            }
            Ok(command)
        })
        .unwrap();
    let tool_queue = queue.clone();
    let run_tool = PyCFunction::new_closure(py, None, None, move |_, _| {
        tool_queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| PyAssertionError::new_err("unexpected external tool call"))
    })
    .unwrap();
    let duplicate_queue = queue;
    let duplicate = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, _| -> PyResult<(i32, Vec<String>, HashMap<String, String>)> {
            check_args(args, 3)?;
            let py = args.py();
            let (returncode, output) = duplicate_queue
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| PyAssertionError::new_err("unexpected duplicate call"))?;
            let issues = args.get_item(1)?;
            let failures = args.get_item(2)?;
            if ![0, 8].contains(&returncode) {
                let kwargs = PyDict::new(py);
                kwargs.set_item("tool", "pylint")?;
                kwargs.set_item("returncode", returncode)?;
                kwargs.set_item("output", &output)?;
                audit(py)
                    .getattr("_record_incomplete_tool")?
                    .call((issues, failures), Some(&kwargs))?;
                return Ok((
                    returncode,
                    Vec::<String>::new(),
                    HashMap::<String, String>::new(),
                ));
            }
            let finding = audit(py).getattr("Issue")?.call1((
                "duplicate_code",
                "medium",
                "multiple",
                py.None(),
                &output,
                "deduplicate",
                PyDict::new(py),
            ))?;
            issues.call_method1("append", (finding,))?;
            Ok((returncode, vec![output], HashMap::<String, String>::new()))
        },
    )
    .unwrap();
    vec![
        AttrPatch::replace(audit(py).as_any(), "_iter_files", iter_files.as_any()),
        AttrPatch::replace(
            audit(py).as_any(),
            "_structural_issues",
            structural.as_any(),
        ),
        AttrPatch::replace(
            audit(py).as_any(),
            "_resolve_tool_command",
            resolve.as_any(),
        ),
        AttrPatch::replace(audit(py).as_any(), "_run_tool", run_tool.as_any()),
        AttrPatch::replace(
            audit(py).as_any(),
            "_pylint_duplicate_issues",
            duplicate.as_any(),
        ),
    ]
}

fn git(root: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for name in GIT_SELECTORS {
        command.env_remove(name);
    }
    let output = command
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn new_repo(case: &Case, name: &str) -> PathBuf {
    let repo = case.mkdir(name);
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &["config", "user.email", "guardrail-tests@example.invalid"],
    );
    git(&repo, &["config", "user.name", "Guardrail Tests"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    repo
}

fn oversized_source(repo: &Path) {
    let assignments: String = (0..105).map(|n| format!("    value = {n}\n")).collect();
    let file = repo.join("research/candidate.py");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, format!("def big():\n{assignments}    return value\n")).unwrap();
}

#[test]
fn iter_files_accepts_explicit_file_target() {
    let case = isolated_case();
    case.write("tools/vault_health.py", "x = 1\n");
    case.write("tools/sibling.py", "y = 2\n");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let kwargs = PyDict::new(py);
        kwargs.set_item("staged_only", false).unwrap();
        let files = audit(py)
            .getattr("_iter_files")
            .unwrap()
            .call((vec!["tools/vault_health.py"],), Some(&kwargs))
            .unwrap();
        let names: Vec<String> = files
            .try_iter()
            .unwrap()
            .map(|p| text(&p.unwrap().getattr("name").unwrap()))
            .collect();
        assert_eq!(names, ["vault_health.py"]);
    });
}

#[test]
fn resolve_tool_command_prefers_running_environment() {
    let case = isolated_case();
    let bin = case.mkdir("bin");
    let python = bin.join("python");
    let tool = bin.join("vulture");
    fs::write(&python, "").unwrap();
    fs::write(&tool, "").unwrap();
    fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    Python::attach(|py| {
        let _executable = AttrPatch::replace(
            module(py, "sys").as_any(),
            "executable",
            python.to_str().unwrap().into_pyobject(py).unwrap().as_any(),
        );
        let command: Vec<String> = audit(py)
            .getattr("_resolve_tool_command")
            .unwrap()
            .call1(("vulture", "research"))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(command, [tool.to_str().unwrap(), "research"]);
    });
}

#[test]
fn run_tool_reports_timeout_without_raising() {
    let _case = isolated_case();
    Python::attach(|py| {
        let timeout =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let kwargs = PyDict::new(py);
                kwargs.set_item("timeout", 7)?;
                kwargs.set_item("output", PyBytes::new(py, b"partial"))?;
                let error = py
                    .import("subprocess")?
                    .getattr("TimeoutExpired")?
                    .call((args.get_item(0)?,), Some(&kwargs))?;
                Err(PyErr::from_value(error))
            })
            .unwrap();
        let _run = AttrPatch::replace(
            audit(py).getattr("subprocess").unwrap().as_any(),
            "run",
            timeout.as_any(),
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout_seconds", 7).unwrap();
        let result = audit(py)
            .getattr("_run_tool")
            .unwrap()
            .call((vec!["pylint", "research"],), Some(&kwargs))
            .unwrap();
        assert_eq!(result.get_item(0).unwrap().extract::<i32>().unwrap(), 124);
        let output = text(&result.get_item(1).unwrap());
        assert!(output.contains("timed out"));
        assert!(output.contains("partial"));
    });
}

fn external_result_case(results: &[(i32, &str)]) -> (Vec<(String, String)>, Value, String) {
    let case = isolated_case();
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let _stubs = stub_external(py, case.root(), results);
        let result = audit(py)
            .getattr("collect_issues")
            .unwrap()
            .call1((PyTuple::new(py, ["research"]).unwrap(),))
            .unwrap();
        let issues = result.get_item(0).unwrap();
        let summary = result.get_item(1).unwrap();
        let kinds = issues
            .try_iter()
            .unwrap()
            .map(|item| {
                let item = item.unwrap();
                (
                    text(&item.getattr("kind").unwrap()),
                    text(&item.getattr("severity").unwrap()),
                )
            })
            .collect();
        let serialized: String = module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call1((&summary,))
            .unwrap()
            .extract()
            .unwrap();
        let report = text(
            &audit(py)
                .getattr("build_markdown_report")
                .unwrap()
                .call1((issues, summary))
                .unwrap(),
        );
        (kinds, serde_json::from_str(&serialized).unwrap(), report)
    })
}

#[test]
fn incomplete_external_tools_fail_closed() {
    let (issues, summary, report) = external_result_case(&[
        (127, "missing tool: vulture"),
        (124, "timed out: pylint research"),
    ]);
    assert_eq!(
        issues,
        vec![("audit_incomplete".to_owned(), "error".to_owned()); 2]
    );
    assert_eq!(summary["audit_complete"], false);
    assert!(summary["dead_code_hits"].is_null());
    assert!(summary["duplicate_hits"].is_null());
    assert!(report.contains("dead code hits reported by vulture: n/a (tool did not complete)"));
    assert!(report
        .contains("duplicate-code hits reported by indexed pylint: n/a (tool did not complete)"));
    assert!(report.contains("critical findings: 0"));
    assert!(report.contains("external tool audit complete: False"));
}

#[test]
fn expected_tool_finding_exit_codes_are_complete() {
    let (issues, summary, _) = external_result_case(&[
        (
            3,
            "research/example.py:1: unused function 'old' (90% confidence)",
        ),
        (8, "R0801: Similar lines in 2 files (duplicate-code)"),
    ]);
    assert_eq!(summary["audit_complete"], true);
    assert_eq!(summary["dead_code_hits"], 1);
    assert_eq!(summary["duplicate_hits"], 1);
    let kinds: HashSet<_> = issues.into_iter().map(|row| row.0).collect();
    assert_eq!(
        kinds,
        ["dead_code".to_owned(), "duplicate_code".to_owned()]
            .into_iter()
            .collect()
    );
}

fn stub_main(
    py: Python<'_>,
    root: &Path,
    kinds: &[(&str, &str)],
    summary: Value,
) -> Vec<AttrPatch> {
    let root = root.to_path_buf();
    let resolve = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        check_args(args, 1)?;
        Ok(path(args.py(), &root).unbind())
    })
    .unwrap();
    let items: Vec<_> = kinds
        .iter()
        .map(|(kind, severity)| issue(py, kind, severity).unbind())
        .collect();
    let collect = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        let py = args.py();
        let issues = PyList::new(py, items.iter().map(|item| item.clone_ref(py)))?;
        let summary = py_json(py, summary.clone());
        Ok(PyTuple::new(py, [issues.into_any(), summary])?
            .into_any()
            .unbind())
    })
    .unwrap();
    vec![
        AttrPatch::replace(audit(py).as_any(), "resolve_audit_root", resolve.as_any()),
        AttrPatch::replace(audit(py).as_any(), "collect_issues", collect.as_any()),
    ]
}

#[test]
fn check_mode_blocks_high_severity_findings() {
    let case = isolated_case();
    let summary = json!({
        "files_scanned":1, "python_files_scanned":1, "dead_code_hits":0,
        "duplicate_hits":0, "audit_complete":true, "tool_failures":[]
    });
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let _stub = stub_main(py, case.root(), &[("complexity", "high")], summary);
        let (_output, _capture) = capture(py, "stdout");
        let code: i32 = audit(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["--check"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
    });
}

#[test]
fn ref_selection_and_structural_parse_fail_closed() {
    let case = isolated_case();
    case.write("probe.txt", "not Python\n");
    case.write("probe.py", "def broken(:\n");
    Python::attach(|py| {
        let value_error = module(py, "builtins").getattr("ValueError").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("staged_only", false).unwrap();
        kwargs.set_item("from_ref", py.None()).unwrap();
        let error = audit(py)
            .getattr("_git_changed_paths")
            .unwrap()
            .call((PyTuple::new(py, ["conductor"]).unwrap(),), Some(&kwargs))
            .unwrap_err();
        assert_error(py, error, &value_error, "select exactly one");
        kwargs.set_item("staged_only", true).unwrap();
        kwargs.set_item("from_ref", "HEAD^").unwrap();
        let error = audit(py)
            .getattr("collect_issues")
            .unwrap()
            .call((PyTuple::new(py, ["conductor"]).unwrap(),), Some(&kwargs))
            .unwrap_err();
        assert_error(py, error, &value_error, "mutually exclusive");
        let _root = patch_root(py, case.root());
        kwargs.set_item("staged_only", false).unwrap();
        kwargs.set_item("from_ref", py.None()).unwrap();
        let files = PyList::new(
            py,
            [
                path(py, &case.root().join("probe.txt")),
                path(py, &case.root().join("probe.py")),
            ],
        )
        .unwrap();
        let result = audit(py)
            .getattr("_structural_issues")
            .unwrap()
            .call((files,), Some(&kwargs))
            .unwrap();
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 1);
        let kinds: Vec<String> = result
            .get_item(0)
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|item| text(&item.unwrap().getattr("kind").unwrap()))
            .collect();
        assert_eq!(kinds, ["syntax_error"]);
    });
}

#[test]
fn candidate_text_has_deterministic_latin1_fallback() {
    let case = isolated_case();
    let file = case.root().join("probe.py");
    fs::write(&file, b"value = '\xff'\n").unwrap();
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let kwargs = PyDict::new(py);
        kwargs.set_item("staged_only", false).unwrap();
        kwargs.set_item("from_ref", py.None()).unwrap();
        let value = audit(py)
            .getattr("_read_candidate_text")
            .unwrap()
            .call((path(py, &file),), Some(&kwargs))
            .unwrap();
        assert!(text(&value).contains('ÿ'));
        let completed =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let kwargs = PyDict::new(py);
                kwargs.set_item("stdout", PyBytes::new(py, b"\xff"))?;
                Ok(py
                    .import("subprocess")?
                    .getattr("CompletedProcess")?
                    .call((Vec::<String>::new(), 0), Some(&kwargs))?
                    .unbind())
            })
            .unwrap();
        let _run = AttrPatch::replace(
            audit(py).getattr("subprocess").unwrap().as_any(),
            "run",
            completed.as_any(),
        );
        kwargs.set_item("staged_only", true).unwrap();
        let staged = audit(py)
            .getattr("_read_candidate_text")
            .unwrap()
            .call((path(py, &file),), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&staged), "ÿ");
    });
}

#[test]
fn explicit_root_scans_the_named_repo_not_cwd() {
    let case = isolated_case();
    let target = new_repo(&case, "target");
    let _decoy = new_repo(&case, "decoy");
    oversized_source(&target);
    git(&target, &["add", "--all"]);
    git(&target, &["commit", "-m", "base"]);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let resolved = audit(py)
            .getattr("resolve_audit_root")
            .unwrap()
            .call1((path(py, &target),))
            .unwrap();
        assert_eq!(
            text(&resolved),
            target.canonicalize().unwrap().to_str().unwrap()
        );
        let (_output, _capture) = capture(py, "stdout");
        let code: i32 = audit(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["--root", target.to_str().unwrap(), "--check"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
    });
}

#[test]
fn default_root_uses_cwd_toplevel_not_module_location() {
    let case = isolated_case();
    let repo = new_repo(&case, "repo");
    oversized_source(&repo);
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "-m", "base"]);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let resolved = audit(py)
            .getattr("resolve_audit_root")
            .unwrap()
            .call1((py.None(),))
            .unwrap();
        assert_eq!(
            text(&resolved),
            repo.canonicalize().unwrap().to_str().unwrap()
        );
        let (_output, _capture) = capture(py, "stdout");
        let code: i32 = audit(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["--check"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
        assert_eq!(
            text(&audit(py).getattr("ROOT").unwrap()),
            repo.canonicalize().unwrap().to_str().unwrap()
        );
    });
}

#[test]
fn cwd_outside_worktree_refuses_rather_than_falling_back() {
    let case = isolated_case();
    let _cwd = case.chdir("not_a_repo");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let (_error, _capture) = capture(py, "stderr");
        let code: i32 = audit(py)
            .getattr("main")
            .unwrap()
            .call1((Vec::<String>::new(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
    });
}

#[test]
fn resolved_root_is_printed() {
    let case = isolated_case();
    let repo = new_repo(&case, "repo");
    git(&repo, &["commit", "--allow-empty", "-m", "base"]);
    Python::attach(|py| {
        let (output, _capture) = capture(py, "stdout");
        let kwargs = PyDict::new(py);
        kwargs.set_item("cwd", path(py, &repo)).unwrap();
        audit(py)
            .getattr("print_audit_provenance")
            .unwrap()
            .call(("guardrail-audit", path(py, &repo)), Some(&kwargs))
            .unwrap();
        assert!(text(&output.call_method0("getvalue").unwrap())
            .contains(&format!("root={}", repo.canonicalize().unwrap().display())));
    });
}

#[test]
fn root_mismatch_warns() {
    let case = isolated_case();
    let target = new_repo(&case, "target");
    let decoy = new_repo(&case, "decoy");
    git(&target, &["commit", "--allow-empty", "-m", "base"]);
    Python::attach(|py| {
        let (error, _capture) = capture(py, "stderr");
        let kwargs = PyDict::new(py);
        kwargs.set_item("cwd", path(py, &decoy)).unwrap();
        audit(py)
            .getattr("print_audit_provenance")
            .unwrap()
            .call(("guardrail-audit", path(py, &target)), Some(&kwargs))
            .unwrap();
        let output = text(&error.call_method0("getvalue").unwrap());
        assert!(output.contains("WARNING"));
        assert!(output.contains(target.canonicalize().unwrap().to_str().unwrap()));
    });
}

fn allowlist<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    audit(py)
        .getattr("_load_allowlist")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
}

#[test]
fn load_allowlist_reads_the_host_copy_not_a_package_copy() {
    let case = isolated_case();
    case.write(
        "conductor/guardrail_allowlist.json",
        &json!({
            "god_files":["only/in/this/host.py"], "god_functions":[], "complexity":[]
        })
        .to_string(),
    );
    Python::attach(|py| {
        let entries: HashSet<String> = allowlist(py, case.root())
            .get_item("god_files")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            entries,
            ["only/in/this/host.py".to_owned()].into_iter().collect()
        );
    });
}

#[test]
fn load_allowlist_is_empty_when_the_host_has_none() {
    let case = isolated_case();
    Python::attach(|py| {
        let result = allowlist(py, case.root());
        for key in ["god_files", "god_functions", "complexity"] {
            assert_eq!(result.get_item(key).unwrap().len().unwrap(), 0);
        }
        assert_eq!(result.len().unwrap(), 3);
    });
}

#[test]
fn load_allowlist_honors_a_conductor_table_override() {
    let case = isolated_case();
    case.write(
        "policy/allow.json",
        &json!({"god_files":["moved.py"],"god_functions":[],"complexity":[]}).to_string(),
    );
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nguardrail_allowlist = \"policy/allow.json\"\n",
    );
    Python::attach(|py| {
        let entries: HashSet<String> = allowlist(py, case.root())
            .get_item("god_files")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(entries, ["moved.py".to_owned()].into_iter().collect());
    });
}

#[test]
fn default_targets_scan_forge_python_and_rust_and_prune_builds() {
    let case = isolated_case();
    case.write("src/conductor/probe.py", "value = 1\n");
    case.write("native/core/src/lib.rs", "// native\n");
    case.write("native/core/target/debug/junk.rs", "// native\n");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let targets = module(py, "conductor.guardrail_targets")
            .getattr("resolve_targets")
            .unwrap()
            .call1((path(py, case.root()), py.None()))
            .unwrap();
        assert_eq!(targets.extract::<Vec<String>>().unwrap(), ["."]);
        let files = audit(py)
            .getattr("_iter_files")
            .unwrap()
            .call1((vec!["."],))
            .unwrap();
        let names: HashSet<String> = files
            .try_iter()
            .unwrap()
            .map(|entry| {
                let relative = entry
                    .unwrap()
                    .call_method1("relative_to", (path(py, case.root()),))
                    .unwrap();
                text(&relative.call_method0("as_posix").unwrap())
            })
            .collect();
        assert_eq!(
            names,
            [
                "src/conductor/probe.py".to_owned(),
                "native/core/src/lib.rs".to_owned()
            ]
            .into_iter()
            .collect()
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("staged_only", false).unwrap();
        kwargs.set_item("from_ref", py.None()).unwrap();
        let result = audit(py)
            .getattr("_structural_issues")
            .unwrap()
            .call((files,), Some(&kwargs))
            .unwrap();
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 1);
        assert_eq!(result.get_item(0).unwrap().len().unwrap(), 0);
    });
}

#[test]
fn host_target_configuration_and_cli_override() {
    let case = isolated_case();
    case.mkdir("source");
    case.mkdir("other");
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nguardrail_targets = [\"source\"]\n",
    );
    Python::attach(|py| {
        let resolve = module(py, "conductor.guardrail_targets")
            .getattr("resolve_targets")
            .unwrap();
        let configured: Vec<String> = resolve
            .call1((path(py, case.root()), py.None()))
            .unwrap()
            .extract()
            .unwrap();
        let explicit: Vec<String> = resolve
            .call1((path(py, case.root()), vec!["other"]))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(configured, ["source"]);
        assert_eq!(explicit, ["other"]);
    });
}

fn invalid_target_case(targets: Value) {
    let case = isolated_case();
    Python::attach(|py| {
        let error = module(py, "conductor.guardrail_targets")
            .getattr("resolve_targets")
            .unwrap()
            .call1((path(py, case.root()), py_json(py, targets)))
            .unwrap_err();
        assert!(error
            .matches(py, &module(py, "builtins").getattr("ValueError").unwrap())
            .unwrap());
    });
}

#[test]
fn invalid_targets_empty_list() {
    invalid_target_case(json!([]));
}
#[test]
fn invalid_targets_missing() {
    invalid_target_case(json!(["missing"]));
}
#[test]
fn invalid_targets_parent_escape() {
    invalid_target_case(json!(["../escape"]));
}
#[test]
fn invalid_targets_absolute() {
    invalid_target_case(json!(["/tmp"]));
}
#[test]
fn invalid_targets_empty_string() {
    invalid_target_case(json!([""]));
}
#[test]
fn invalid_targets_string_not_sequence() {
    invalid_target_case(json!("source"));
}

#[test]
fn no_python_and_scoped_reports_do_not_claim_zero_tool_findings() {
    let case = isolated_case();
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let empty =
            PyCFunction::new_closure(py, None, None, move |_, _| -> PyResult<Vec<String>> {
                Ok(Vec::new())
            })
            .unwrap();
        let _iter = AttrPatch::replace(audit(py).as_any(), "_iter_files", empty.as_any());
        let whole = audit(py)
            .getattr("collect_issues")
            .unwrap()
            .call1((vec!["."],))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("staged_only", true).unwrap();
        let scoped = audit(py)
            .getattr("collect_issues")
            .unwrap()
            .call((vec!["."],), Some(&kwargs))
            .unwrap();
        let whole_summary = whole.get_item(1).unwrap();
        let scoped_summary = scoped.get_item(1).unwrap();
        assert_eq!(
            text(&whole_summary.get_item("vulture_status").unwrap()),
            "not_applicable"
        );
        assert_eq!(
            text(&scoped_summary.get_item("vulture_status").unwrap()),
            "not_run_scoped"
        );
        assert!(whole_summary.get_item("dead_code_hits").unwrap().is_none());
        assert!(scoped_summary.get_item("duplicate_hits").unwrap().is_none());
        let report = audit(py).getattr("build_markdown_report").unwrap();
        assert!(
            text(&report.call1((Vec::<String>::new(), whole_summary)).unwrap())
                .contains("no eligible Python files")
        );
        assert!(text(
            &report
                .call1((Vec::<String>::new(), scoped_summary))
                .unwrap()
        )
        .contains("not run in scoped audit"));
    });
}

#[test]
fn incomplete_audit_returns_error_without_code_critical() {
    let case = isolated_case();
    let summary = json!({
        "files_scanned":1, "python_files_scanned":1, "dead_code_hits":null,
        "duplicate_hits":null, "audit_complete":false, "tool_failures":["missing tool"]
    });
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let _stub = stub_main(py, case.root(), &[], summary);
        let (_out, _capture) = capture(py, "stdout");
        let code: i32 = audit(py)
            .getattr("main")
            .unwrap()
            .call1((Vec::<String>::new(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
    });
}

#[test]
fn duplicate_failure_is_unavailable_in_external_summary() {
    let case = isolated_case();
    let file = case.write("probe.py", "value = 1\n");
    Python::attach(|py| {
        let _root = patch_root(py, case.root());
        let no_hits =
            PyCFunction::new_closure(py, None, None, move |_, _| -> PyResult<(i32, String)> {
                Ok((0, String::new()))
            })
            .unwrap();
        let _run = AttrPatch::replace(audit(py).as_any(), "_run_tool", no_hits.as_any());
        let fail = PyCFunction::new_closure(py, None, None, move |_, _| -> PyResult<Py<PyAny>> {
            Err(pyo3::exceptions::PyValueError::new_err(
                "cannot normalize selected source",
            ))
        })
        .unwrap();
        let _scan = AttrPatch::replace(
            module(py, "conductor.guardrail_duplicates").as_any(),
            "scan_duplicates",
            fail.as_any(),
        );
        let issues = PyList::empty(py);
        let result = audit(py)
            .getattr("_external_issues")
            .unwrap()
            .call1((vec!["."], vec![path(py, &file)], &issues))
            .unwrap();
        assert!(result.get_item("duplicate_hits").unwrap().is_none());
        assert_eq!(
            text(&result.get_item("pylint_status").unwrap()),
            "incomplete"
        );
        assert_eq!(
            result
                .get_item("dead_code_hits")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
        assert!(!result
            .get_item("audit_complete")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let first = issues.get_item(0).unwrap();
        assert_eq!(text(&first.getattr("kind").unwrap()), "audit_incomplete");
        assert_eq!(text(&first.getattr("severity").unwrap()), "error");
    });
}
