#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_tooling_boundary.py (40 expanded cases).

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyList};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn source_package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src/conductor")
        .canonicalize()
        .unwrap()
}

fn tree(case: &Case, files: &[(&str, &str)]) -> PathBuf {
    tree_at(case.root(), files)
}

fn tree_at(root: &Path, files: &[(&str, &str)]) -> PathBuf {
    for (relative, body) in files {
        let target = root.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, body).unwrap();
    }
    root.join("conductor")
}

fn test_constants(py: Python<'_>) -> (String, String) {
    let api = module(py, "conductor.tooling_boundary");
    let packages = api.getattr("PROJECT_PACKAGES").unwrap();
    (
        text(&packages.get_item(0).unwrap()),
        text(&api.getattr("NATIVE_CRATE").unwrap()),
    )
}

fn violations<'py>(
    py: Python<'py>,
    api: &Bound<'py, PyModule>,
    method: &str,
    package: &Path,
) -> Vec<Bound<'py, PyAny>> {
    api.getattr(method)
        .unwrap()
        .call1((path(py, package),))
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn violation_lines(rows: &[Bound<'_, PyAny>]) -> BTreeSet<(String, i64)> {
    rows.iter()
        .map(|row| {
            (
                text(&row.getattr("path").unwrap()),
                row.getattr("line").unwrap().extract().unwrap(),
            )
        })
        .collect()
}

fn violation_strings(rows: &[Bound<'_, PyAny>]) -> Vec<String> {
    rows.iter().map(text).collect()
}

fn config<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("rootpath", path(py, root)).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

struct DictPatch {
    mapping: Py<PyAny>,
    key: String,
    prior: Option<Py<PyAny>>,
}

impl DictPatch {
    fn insert(mapping: &Bound<'_, PyAny>, key: &str, value: &Bound<'_, PyAny>) -> Self {
        let prior = mapping
            .cast::<PyDict>()
            .unwrap()
            .get_item(key)
            .unwrap()
            .map(|value| value.unbind());
        mapping.set_item(key, value).unwrap();
        Self {
            mapping: mapping.clone().unbind(),
            key: key.to_owned(),
            prior,
        }
    }
}

impl Drop for DictPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            let mapping = self.mapping.bind(py);
            match &self.prior {
                Some(value) => mapping.set_item(&self.key, value.bind(py)).unwrap(),
                None => {
                    mapping.call_method1("pop", (&self.key, py.None())).unwrap();
                }
            }
        });
    }
}

struct Captured {
    out: Py<PyAny>,
    err: Py<PyAny>,
    out_context: Py<PyAny>,
    err_context: Py<PyAny>,
    active: bool,
}

impl Captured {
    fn enter(py: Python<'_>) -> Self {
        let io = module(py, "io");
        let out = io.getattr("StringIO").unwrap().call0().unwrap();
        let err = io.getattr("StringIO").unwrap().call0().unwrap();
        let contextlib = module(py, "contextlib");
        let out_context = contextlib
            .getattr("redirect_stdout")
            .unwrap()
            .call1((&out,))
            .unwrap();
        let err_context = contextlib
            .getattr("redirect_stderr")
            .unwrap()
            .call1((&err,))
            .unwrap();
        out_context.call_method0("__enter__").unwrap();
        err_context.call_method0("__enter__").unwrap();
        Self {
            out: out.unbind(),
            err: err.unbind(),
            out_context: out_context.unbind(),
            err_context: err_context.unbind(),
            active: true,
        }
    }
    fn finish(mut self, py: Python<'_>) -> (String, String) {
        self.restore(py);
        let stdout = self
            .out
            .bind(py)
            .call_method0("getvalue")
            .unwrap()
            .extract()
            .unwrap();
        let stderr = self
            .err
            .bind(py)
            .call_method0("getvalue")
            .unwrap()
            .extract()
            .unwrap();
        (stdout, stderr)
    }
    fn restore(&mut self, py: Python<'_>) {
        if self.active {
            self.err_context
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
            self.out_context
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
            self.active = false;
        }
    }
}

impl Drop for Captured {
    fn drop(&mut self) {
        Python::attach(|py| self.restore(py));
    }
}

fn configure_plugin_env(case: &mut Case, value: Option<&str>) {
    const ENV: &str = "CONDUCTOR_PROJECT_TEST_PLUGIN";
    match value {
        Some(value) => case.set_env(ENV, value),
        None => case.remove_env(ENV),
    }
}

fn call_check_all<'py>(py: Python<'py>, package: &Path) -> PyResult<Bound<'py, PyAny>> {
    module(py, "conductor.tooling_boundary")
        .getattr("check_all")?
        .call1((path(py, package),))
}

#[test]
fn rule_a_no_conductor_module_reaches_a_project_package() {
    let case = Case::new();
    Python::attach(|py| {
        let package = source_package();
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert!(violation_strings(&rows).is_empty());
    });
    drop(case);
}

#[test]
fn rule_b_generic_hooks_carry_no_project_literal() {
    let case = Case::new();
    Python::attach(|py| {
        let api = module(py, "conductor.tooling_boundary");
        let package = source_package();
        let dirs = api
            .getattr("default_hook_dirs")
            .unwrap()
            .call1((path(py, &package),))
            .unwrap();
        assert!(
            dirs.len().unwrap() > 0,
            "no hook tree found next to the package"
        );
        let rows = api
            .getattr("check_hook_literals")
            .unwrap()
            .call1((dirs, path(py, &package)))
            .unwrap();
        assert!(violation_strings(
            &rows
                .try_iter()
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        )
        .is_empty());
    });
    drop(case);
}

#[test]
fn rule_c_non_test_modules_carry_no_host_path_literal() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_path_literals",
            &source_package(),
        );
        assert!(violation_strings(&rows).is_empty());
    });
    drop(case);
}

#[test]
fn rule_d_native_seam_is_the_only_seam_and_exports_real_symbols() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_native_seam",
            &source_package(),
        );
        assert!(violation_strings(&rows).is_empty());
    });
    drop(case);
}

#[test]
fn cli_reports_clean_on_this_repo() {
    let case = Case::new();
    Python::attach(|py| {
        let root = source_package()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let captured = Captured::enter(py);
        let args = PyList::new(py, ["--root", root.to_str().unwrap()]).unwrap();
        let status = module(py, "conductor.tooling_boundary")
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        let (stdout, _) = captured.finish(py);
        assert_eq!(status, 0);
        assert!(stdout.contains("findings=0") && stdout.contains("rule d: clean"));
    });
    drop(case);
}

#[test]
fn repo_root_is_the_tree_that_declares_the_package() {
    let case = Case::new();
    Python::attach(|py| {
        let api = module(py, "conductor.project_paths");
        let package = source_package();
        let root = api
            .getattr("package_tree_root")
            .unwrap()
            .call1((path(py, &package),))
            .unwrap();
        let repo = package.parent().unwrap().parent().unwrap();
        assert_eq!(text(&root), repo.to_string_lossy());
        let relative = api
            .getattr("package_path")
            .unwrap()
            .call1((root.clone(),))
            .unwrap();
        let actual = root
            .call_method1("joinpath", (relative,))
            .unwrap()
            .call_method0("resolve")
            .unwrap();
        assert!(actual.eq(path(py, &package)).unwrap());
        assert!(repo.join("pyproject.toml").is_file());
    });
    drop(case);
}

#[test]
fn cli_refuses_a_root_with_no_package_naming_the_configured_path() {
    let case = Case::new();
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor]\npackage_root = \"src/conductor\"\n",
    )
    .unwrap();
    Python::attach(|py| {
        let captured = Captured::enter(py);
        let args = PyList::new(py, ["--root", case.root().to_str().unwrap()]).unwrap();
        let status = module(py, "conductor.tooling_boundary")
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        let (_, stderr) = captured.finish(py);
        assert_eq!(status, 2);
        assert!(stderr.contains("no src/conductor/ under"));
    });
}

#[test]
fn cli_finds_a_package_the_tree_declares_under_src() {
    let case = Case::new();
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor]\npackage_root = \"src/conductor\"\n",
    )
    .unwrap();
    let package = case.mkdir("src/conductor");
    let seen = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    Python::attach(|py| {
        let api = module(py, "conductor.tooling_boundary");
        let seen_callback = Arc::clone(&seen);
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, _kwargs| -> PyResult<Py<PyAny>> {
                let directory = args.get_item(0)?;
                seen_callback
                    .lock()
                    .unwrap()
                    .push(PathBuf::from(directory.str()?.to_str()?));
                let result = PyDict::new(args.py());
                result.set_item("a", PyList::empty(args.py()))?;
                Ok(result.unbind().into_any())
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(api.as_any(), "check_all", callback.as_any());
        let captured = Captured::enter(py);
        let args = PyList::new(py, ["--root", case.root().to_str().unwrap()]).unwrap();
        let status = api
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        let _ = captured.finish(py);
        assert_eq!(status, 0);
        assert_eq!(*seen.lock().unwrap(), vec![package]);
    });
}

#[test]
fn allowlist_entries_carry_a_reason_and_name_existing_files() {
    let case = Case::new();
    Python::attach(|py| {
        let api = module(py, "conductor.tooling_boundary");
        let package = source_package();
        for row in api.getattr("ALLOWLIST").unwrap().try_iter().unwrap() {
            let row = row.unwrap();
            let rel = text(&row.get_item(0).unwrap());
            let kind = text(&row.get_item(1).unwrap());
            let value = text(&row.get_item(2).unwrap());
            let reason = text(&row.get_item(3).unwrap());
            assert!(package.join(rel).is_file());
            assert!(matches!(kind.as_str(), "import" | "string" | "literal"));
            assert!(!value.is_empty() && !reason.is_empty());
        }
    });
    drop(case);
}

#[test]
fn rule_a_flags_module_scope_import_with_file_and_line() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let package = tree(
            &case,
            &[(
                "conductor/x.py",
                &format!("import os\nimport {project}.tools\n"),
            )],
        );
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert_eq!(
            violation_strings(&rows),
            vec![format!(
                "conductor/x.py:2: [a] imports project module {project}.tools"
            )]
        );
    });
}

#[test]
fn rule_a_flags_function_scope_and_try_except_imports() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let second = module(py, "conductor.tooling_boundary")
            .getattr("PROJECT_PACKAGES")
            .unwrap()
            .get_item(1)
            .unwrap();
        let source = format!("\ndef f():\n    from {project}.tools.thing import X\n    return X\ntry:\n    import {}\nexcept ImportError:\n    pass\n", text(&second));
        let package = tree(&case, &[("conductor/x.py", &source)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert_eq!(
            violation_lines(&rows),
            [("conductor/x.py".into(), 3), ("conductor/x.py".into(), 6)]
                .into_iter()
                .collect()
        );
    });
}

#[test]
fn rule_a_flags_importlib_string_literal() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let source = format!("\nimport importlib\ndef load():\n    return importlib.import_module(\"{project}.tools.thing\")\n");
        let package = tree(&case, &[("conductor/x.py", &source)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].getattr("line").unwrap().extract::<i64>().unwrap(),
            4
        );
        assert!(
            text(&rows[0].getattr("message").unwrap()).contains(&format!("{project}.tools.thing"))
        );
    });
}

#[test]
fn rule_a_ignores_non_module_strings() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let source = format!("\nA = \"{project}.tools.thing train\"\nB = \"{project}/notes/kb.md\"\nC = \"{project}\"\nD = \"{project}.x\"\nE = \"{project}:fn\"\n");
        let package = tree(&case, &[("conductor/x.py", &source)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        let lines = rows
            .iter()
            .map(|v| v.getattr("line").unwrap().extract::<i64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines, vec![5, 6]);
    });
}

#[test]
fn rule_a_rejects_host_plugin_string_in_generic_module() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let plugin = format!("{project}.tests._path_guard:register");
        let first = format!("PLUGIN = \"{plugin}\"\n");
        let package = tree(
            &case,
            &[
                ("conductor/_project_hooks.py", &first),
                ("conductor/other.py", &first),
            ],
        );
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert_eq!(
            violation_lines(&rows),
            [
                ("conductor/_project_hooks.py".into(), 1),
                ("conductor/other.py".into(), 1)
            ]
            .into_iter()
            .collect()
        );
    });
}

#[test]
fn rule_a_scans_tests_too() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, _) = test_constants(py);
        let source = format!("import {project}\n");
        let package = tree(&case, &[("conductor/tests/test_x.py", &source)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_project_imports",
            &package,
        );
        assert_eq!(
            violation_lines(&rows),
            [("conductor/tests/test_x.py".into(), 1)]
                .into_iter()
                .collect()
        );
    });
}

fn rule_b_literal(literal: &str) {
    let case = Case::new();
    Python::attach(|py| {
        let shell = format!("#!/bin/sh\necho ok\nls {literal}x\n");
        let project_hook = format!("export X={literal}\n");
        let py_hook = format!("X = '{literal}'\n");
        let cache = format!("{literal}/pre.py\n");
        let agent_hook = format!("P = '{literal}'\n");
        let package = tree(
            &case,
            &[
                ("conductor/x.py", ""),
                (".claude/hooks/pre.sh", &shell),
                (".claude/hooks/project/env.sh", &project_hook),
                (".claude/hooks/test_pre.py", &py_hook),
                (".claude/hooks/__pycache__/pre.cpython-312.pyc", &cache),
                (".agent_hooks/guard.py", &agent_hook),
            ],
        );
        let api = module(py, "conductor.tooling_boundary");
        let dirs = api
            .getattr("default_hook_dirs")
            .unwrap()
            .call1((path(py, &package),))
            .unwrap();
        let rows = api
            .getattr("check_hook_literals")
            .unwrap()
            .call1((dirs, path(py, &package)))
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(
            violation_lines(&rows),
            [
                (".claude/hooks/pre.sh".into(), 3),
                (".agent_hooks/guard.py".into(), 1)
            ]
            .into_iter()
            .collect()
        );
        assert!(rows
            .iter()
            .all(|row| text(&row.getattr("message").unwrap()).contains(&format!("'{literal}'"))));
    });
}
macro_rules! rule_b_case {
    ($name:ident, $value:literal) => {
        #[test]
        fn $name() {
            rule_b_literal($value);
        }
    };
}
rule_b_case!(rule_b_flags_research_literal, "research/");
rule_b_case!(rule_b_flags_home_literal, "/home/tim");
rule_b_case!(rule_b_flags_data_literal, "/mnt/data");

#[test]
fn rule_b_default_hook_dirs_cover_repo_and_standalone_layouts() {
    let case = Case::new();
    Python::attach(|py| {
        let api = module(py, "conductor.tooling_boundary");
        tree(
            &case,
            &[
                ("src/conductor/x.py", ""),
                ("hooks/pre.sh", "ls /mnt/data\n"),
            ],
        );
        let dirs = api
            .getattr("default_hook_dirs")
            .unwrap()
            .call1((path(py, &case.root().join("src/conductor")),))
            .unwrap();
        assert_eq!(
            text(&dirs.get_item(0).unwrap()),
            case.root().join("hooks").to_string_lossy()
        );
        assert_eq!(dirs.len().unwrap(), 1);
        let moved_root = case.root().join("moved/repo");
        let moved = tree_at(
            &moved_root,
            &[
                ("conductor/x.py", ""),
                ("tooling/hooks/claude/pre.sh", "ls /mnt/data\n"),
            ],
        );
        let dirs = api
            .getattr("default_hook_dirs")
            .unwrap()
            .call1((path(py, &moved),))
            .unwrap();
        assert_eq!(
            text(&dirs.get_item(0).unwrap()),
            moved_root.join("tooling/hooks").to_string_lossy()
        );
        assert_eq!(dirs.len().unwrap(), 1);
        let nowhere = case.root().join("a/b/conductor");
        assert_eq!(
            api.getattr("default_hook_dirs")
                .unwrap()
                .call1((path(py, &nowhere),))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        assert_error(
            py,
            call_check_all(py, &nowhere).unwrap_err(),
            &module(py, "builtins").getattr("FileNotFoundError").unwrap(),
            "no hook tree",
        );
    });
}

fn rule_c_literal(literal: &str) {
    let case = Case::new();
    Python::attach(|py| {
        let ordinary = format!("# {literal} in a comment counts\nP = '{literal}/x'\n");
        let test_one = format!("P = '{literal}'\n");
        let package = tree(
            &case,
            &[
                ("conductor/x.py", &ordinary),
                ("conductor/test_x.py", &test_one),
                ("conductor/tests/test_y.py", &test_one),
            ],
        );
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_path_literals",
            &package,
        );
        assert_eq!(
            violation_lines(&rows),
            [("conductor/x.py".into(), 1), ("conductor/x.py".into(), 2)]
                .into_iter()
                .collect()
        );
    });
}
macro_rules! rule_c_case {
    ($name:ident, $value:literal) => {
        #[test]
        fn $name() {
            rule_c_literal($value);
        }
    };
}
rule_c_case!(rule_c_flags_home_literal, "/home/tim");
rule_c_case!(rule_c_flags_data_literal, "/mnt/data");

#[test]
fn rule_d_flags_a_symbol_the_crate_does_not_export() {
    let case = Case::new();
    Python::attach(|py| {
        let (_, native) = test_constants(py);
        let seam = format!("from {native} import (\n    validate_mutation_receipt_native,\n    definitely_not_a_symbol_native,\n)\n");
        let package = tree(&case, &[("conductor/_native.py", &seam)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_native_seam",
            &package,
        );
        assert_eq!(violation_strings(&rows), vec![format!("conductor/_native.py:1: [d] definitely_not_a_symbol_native is not exported by {native}")]);
    });
}

#[test]
fn rule_d_flags_the_crate_named_outside_the_seam() {
    let case = Case::new();
    Python::attach(|py| {
        let (_, native) = test_constants(py);
        let seam = fs::read_to_string(source_package().join("_native.py")).unwrap();
        let x = format!("\ndef f():\n    import {native}\n    return {native}\n");
        let y = format!("import importlib\nm = importlib.import_module(\"{native}\")\n");
        let package = tree(
            &case,
            &[
                ("conductor/_native.py", &seam),
                ("conductor/x.py", &x),
                ("conductor/y.py", &y),
            ],
        );
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_native_seam",
            &package,
        );
        assert_eq!(
            violation_lines(&rows),
            [("conductor/x.py".into(), 3), ("conductor/y.py".into(), 2)]
                .into_iter()
                .collect()
        );
    });
}

#[test]
fn rule_d_flags_a_seam_importing_anything_but_the_crate() {
    let case = Case::new();
    Python::attach(|py| {
        let (project, native) = test_constants(py);
        let seam =
            format!("import {project}\nfrom {native} import validate_mutation_receipt_native\n");
        let package = tree(&case, &[("conductor/_native.py", &seam)]);
        let rows = violations(
            py,
            &module(py, "conductor.tooling_boundary"),
            "check_native_seam",
            &package,
        );
        assert_eq!(
            violation_strings(&rows),
            vec![format!(
                "conductor/_native.py:1: [d] seam imports {project}, not the crate"
            )]
        );
    });
}

#[test]
fn project_hooks_unset_without_configuration_resolves_none() {
    let mut case = Case::new();
    configure_plugin_env(&mut case, None);
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        assert!(hooks
            .getattr("resolve_test_plugin")
            .unwrap()
            .call1((py.None(),))
            .unwrap()
            .is_none());
        let calls = Arc::new(Mutex::new(Vec::<(Option<String>, String)>::new()));
        let captured = Arc::clone(&calls);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let spec = args.get_item(0)?;
                let source = kwargs
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?
                    .get_item("source")?
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?;
                captured.lock().unwrap().push((
                    if spec.is_none() {
                        None
                    } else {
                        Some(spec.extract()?)
                    },
                    source.extract()?,
                ));
                Ok(args.py().None())
            })
            .unwrap();
        let _patch = AttrPatch::replace(hooks.as_any(), "resolve_test_plugin", callback.as_any());
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((config(py, case.root()),))
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(None, "[tool.conductor.pytest].test_plugin".into())]
        );
    });
}

#[test]
fn project_hooks_invokes_configured_callable() {
    let mut case = Case::new();
    configure_plugin_env(&mut case, None);
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor.pytest]\ntest_plugin = \"test_host_plugin:register\"\n",
    )
    .unwrap();
    Python::attach(|py| {
        let host = module(py, "types")
            .getattr("ModuleType")
            .unwrap()
            .call1(("test_host_plugin",))
            .unwrap();
        let received = PyList::empty(py);
        host.setattr("register", received.getattr("append").unwrap())
            .unwrap();
        let modules = module(py, "sys").getattr("modules").unwrap();
        let _mapping = DictPatch::insert(&modules, "test_host_plugin", &host);
        let cfg = config(py, case.root());
        module(py, "conductor._project_hooks")
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((&cfg,))
            .unwrap();
        assert_eq!(received.len(), 1);
        assert!(received.get_item(0).unwrap().is(&cfg));
    });
}

#[test]
fn project_hooks_environment_bypasses_malformed_config() {
    let mut case = Case::new();
    fs::write(case.root().join("pyproject.toml"), "[tool").unwrap();
    configure_plugin_env(&mut case, Some(""));
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((config(py, case.root()),))
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::<(Option<String>, String)>::new()));
        let captured = Arc::clone(&calls);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let spec = args.get_item(0)?;
                let source = kwargs
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?
                    .get_item("source")?
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?;
                captured
                    .lock()
                    .unwrap()
                    .push((Some(spec.extract()?), source.extract()?));
                Ok(args.py().None())
            })
            .unwrap();
        let _patch = AttrPatch::replace(hooks.as_any(), "resolve_test_plugin", callback.as_any());
        case.set_env("CONDUCTOR_PROJECT_TEST_PLUGIN", "override.plugin:register");
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((config(py, case.root()),))
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(
                Some("override.plugin:register".into()),
                "CONDUCTOR_PROJECT_TEST_PLUGIN".into()
            )]
        );
    });
}

#[test]
fn project_hooks_refuse_unreadable_or_oversized_config() {
    let case = Case::new();
    let config_path = case.root().join("pyproject.toml");
    fs::write(&config_path, "[tool]\n").unwrap();
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        // pathlib.Path.open delegates to io.open; patch the callable there so
        // the C closure receives the Path argument rather than relying on
        // descriptor binding for a PyCFunction installed on Path.
        let io = module(py, "io");
        let original = io.getattr("open").unwrap().unbind();
        let target = path(py, &config_path).unbind();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let this = args.get_item(0)?;
                if this.eq(target.bind(args.py()))? {
                    return Err(pyo3::exceptions::PyPermissionError::new_err("denied"));
                }
                Ok(original.bind(args.py()).call(args, kwargs)?.unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(io.as_any(), "open", callback.as_any());
        assert_error(
            py,
            hooks
                .getattr("_configured_test_plugin")
                .unwrap()
                .call1((config(py, case.root()),))
                .unwrap_err(),
            &module(py, "builtins").getattr("ValueError").unwrap(),
            "cannot read configuration",
        );
        drop(_patch);
        let limit: usize = hooks.getattr("_CONFIG_LIMIT").unwrap().extract().unwrap();
        fs::write(&config_path, "x".repeat(limit + 1)).unwrap();
        assert_error(
            py,
            hooks
                .getattr("_configured_test_plugin")
                .unwrap()
                .call1((config(py, case.root()),))
                .unwrap_err(),
            &module(py, "builtins").getattr("ValueError").unwrap(),
            "exceeds 64 KiB",
        );
    });
}

fn missing_section(content: &str) {
    let case = Case::new();
    fs::write(case.root().join("pyproject.toml"), content).unwrap();
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        let result = hooks
            .getattr("_configured_test_plugin")
            .unwrap()
            .call1((config(py, case.root()),))
            .unwrap();
        assert!(result.get_item(0).unwrap().is_none());
    });
}
macro_rules! missing_section_case {
    ($name:ident, $content:literal) => {
        #[test]
        fn $name() {
            missing_section($content);
        }
    };
}
missing_section_case!(missing_config_file_content_is_none, "");
missing_section_case!(missing_tool_section_is_none, "[tool]\n");
missing_section_case!(missing_conductor_section_is_none, "[tool.conductor]\n");
missing_section_case!(missing_pytest_section_is_none, "[tool.conductor.pytest]\n");

#[test]
fn project_hooks_config_and_environment_precedence() {
    let mut case = Case::new();
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor.pytest]\ntest_plugin = \"host.plugin:register\"\n",
    )
    .unwrap();
    configure_plugin_env(&mut case, None);
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        let calls = Arc::new(Mutex::new(Vec::<(Option<String>, String)>::new()));
        let captured = Arc::clone(&calls);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let spec = args.get_item(0)?;
                let source = kwargs
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?
                    .get_item("source")?
                    .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("source"))?;
                captured.lock().unwrap().push((
                    if spec.is_none() {
                        None
                    } else {
                        Some(spec.extract()?)
                    },
                    source.extract()?,
                ));
                Ok(args.py().None())
            })
            .unwrap();
        let _patch = AttrPatch::replace(hooks.as_any(), "resolve_test_plugin", callback.as_any());
        let cfg = config(py, case.root());
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((&cfg,))
            .unwrap();
        case.set_env("CONDUCTOR_PROJECT_TEST_PLUGIN", "");
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((&cfg,))
            .unwrap();
        case.set_env("CONDUCTOR_PROJECT_TEST_PLUGIN", "override.plugin:register");
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((&cfg,))
            .unwrap();
        let rows = calls.lock().unwrap().clone();
        assert_eq!(rows[0].0.as_deref(), Some("host.plugin:register"));
        assert!(rows[0].1.ends_with("[tool.conductor.pytest].test_plugin"));
        assert_eq!(
            rows[1..],
            [
                (Some("".into()), "CONDUCTOR_PROJECT_TEST_PLUGIN".into()),
                (
                    Some("override.plugin:register".into()),
                    "CONDUCTOR_PROJECT_TEST_PLUGIN".into()
                )
            ]
        );
    });
}

fn malformed_config(content: &str, expected: &str) {
    let case = Case::new();
    fs::write(case.root().join("pyproject.toml"), content).unwrap();
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        let error = hooks
            .getattr("_configured_test_plugin")
            .unwrap()
            .call1((config(py, case.root()),))
            .unwrap_err();
        let value_error = module(py, "builtins").getattr("ValueError").unwrap();
        assert_error(py, error, &value_error, expected);
    });
}
macro_rules! malformed_config_case {
    ($name:ident, $content:literal, $match:literal) => {
        #[test]
        fn $name() {
            malformed_config($content, $match);
        }
    };
}
malformed_config_case!(
    malformed_toml_is_refused,
    "[tool",
    "invalid TOML configuration"
);
malformed_config_case!(
    non_table_conductor_is_refused,
    "[tool]\nconductor = []\n",
    "[tool.conductor] must be a table"
);
malformed_config_case!(
    non_table_pytest_is_refused,
    "[tool.conductor]\npytest = []\n",
    "[tool.conductor.pytest] must be a table"
);
malformed_config_case!(
    non_string_plugin_is_refused,
    "[tool.conductor.pytest]\ntest_plugin = 1\n",
    "test_plugin must be a string"
);

#[test]
fn project_hooks_refuse_noncallable_configured_attribute() {
    let mut case = Case::new();
    configure_plugin_env(&mut case, None);
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor.pytest]\ntest_plugin = \"conductor._project_hooks:PLUGIN_ENV\"\n",
    )
    .unwrap();
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        assert_error(
            py,
            hooks
                .getattr("register_test_path_guard")
                .unwrap()
                .call1((config(py, case.root()),))
                .unwrap_err(),
            &module(py, "builtins").getattr("TypeError").unwrap(),
            "not callable",
        );
    });
}

#[test]
fn project_hooks_empty_spec_means_no_guard() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_PROJECT_TEST_PLUGIN", "");
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        assert!(hooks
            .getattr("resolve_test_plugin")
            .unwrap()
            .call1(("",))
            .unwrap()
            .is_none());
        hooks
            .getattr("register_test_path_guard")
            .unwrap()
            .call1((py.None(),))
            .unwrap();
    });
}

#[test]
fn project_hooks_bogus_spec_fails_loud() {
    let mut case = Case::new();
    Python::attach(|py| {
        let hooks = module(py, "conductor._project_hooks");
        let import_error = module(py, "builtins").getattr("ImportError").unwrap();
        for spec in [
            "no_such_module_xyz:register",
            "conductor.atomic_json:no_such_fn",
        ] {
            case.set_env("CONDUCTOR_PROJECT_TEST_PLUGIN", spec);
            assert_error(
                py,
                hooks
                    .getattr("register_test_path_guard")
                    .unwrap()
                    .call1((py.None(),))
                    .unwrap_err(),
                &import_error,
                "CONDUCTOR_PROJECT_TEST_PLUGIN",
            );
        }
        assert_error(
            py,
            hooks
                .getattr("resolve_test_plugin")
                .unwrap()
                .call1(("conductor.atomic_json",))
                .unwrap_err(),
            &module(py, "builtins").getattr("ValueError").unwrap(),
            "module:function",
        );
    });
}
