#![cfg(feature = "python-compat-tests")]
//! Rust assertions for exact mutation-run ownership and Mull path allowlists.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_coverage_scope_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{changed_callback, equal, scope_campaign, set_sources, strict_callback};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn scope<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_run_scope").into_any()
}

fn error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    assert_error(
        py,
        result.unwrap_err(),
        &module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap(),
        part,
    );
}

fn validate<'py>(
    py: Python<'py>,
    manifest: &Bound<'py, PyAny>,
    root: &Path,
    only: Option<&[&str]>,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root)).unwrap();
    if let Some(only) = only {
        kw.set_item("only", PyList::new(py, only).unwrap()).unwrap();
    }
    scope(py)
        .getattr("validate_run_scope")
        .unwrap()
        .call((manifest,), Some(&kw))
}

fn changed<'py>(py: Python<'py>, scope: &Bound<'py, PyAny>, paths: &[&str]) -> AttrPatch {
    AttrPatch::replace(
        scope,
        "changed_sources",
        changed_callback(py, paths).as_any(),
    )
}

fn assert_run_cli_refuses_before_engine(py: Python<'_>, root: &Path) {
    let broad = scope_campaign(py, root, &["mine.py", "other.py"], "fest").unbind();
    let runner = module(py, "conductor.mutation_engine_generated");
    let _root_patch = AttrPatch::replace(runner.as_any(), "REPO_ROOT", &path(py, root));
    let load = strict_callback(py, &["path"], &[], move |py, _args| Ok(broad.clone_ref(py)));
    let _load_patch = AttrPatch::replace(runner.as_any(), "load_generated_campaign", load.as_any());
    let engine_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&engine_calls);
    let adapter = strict_callback(py, &["engine"], &[], move |_py, args| {
        seen.lock()
            .unwrap()
            .push(args.get_item("engine")?.extract()?);
        Err(pyo3::exceptions::PyRuntimeError::new_err("engine reached"))
    });
    let _adapter_patch = AttrPatch::replace(runner.as_any(), "adapter_for", adapter.as_any());
    let (output, _stdout) = comm_support::capture(py, "stdout");
    let args = PyList::new(
        py,
        [
            "run",
            "campaign.json",
            "--allow-mutations",
            "--owner",
            "agent",
        ],
    )
    .unwrap();
    assert_eq!(
        runner
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract::<i64>()
            .unwrap(),
        4
    );
    let verdict = py
        .import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((comm_support::buffer_text(&output),))
        .unwrap();
    assert_eq!(
        verdict
            .get_item("status")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        "REFUSED"
    );
    assert!(engine_calls.lock().unwrap().is_empty());
}

#[test]
fn test_agent_scope_and_explicit_selection_cannot_admit_a_neighbour() {
    let case = Case::new();
    Python::attach(|py| {
        let root = case.root();
        let subject = scope(py);
        let manifest = scope_campaign(py, root, &["mine.py", "other.py"], "fest");
        let mut patch = changed(py, &subject, &["mine.py"]);
        error(
            py,
            validate(py, &manifest, root, None),
            "outside this agent's changes",
        );
        error(
            py,
            validate(py, &manifest, root, Some(&["other.py"])),
            "--only names files outside",
        );
        set_sources(py, &manifest, &["mine.py"]);
        equal(
            &validate(py, &manifest, root, Some(&["mine.py"])).unwrap(),
            &PyList::new(py, ["mine.py"]).unwrap(),
        );
        set_sources(py, &manifest, &["mine.py", "other.py"]);
        drop(patch);
        patch = changed(py, &subject, &["mine.py", "other.py"]);
        error(
            py,
            validate(py, &manifest, root, Some(&["mine.py"])),
            "other.py",
        );
        drop(patch);

        let calls = Arc::new(Mutex::new(Vec::<(String, Py<PyAny>, String)>::new()));
        let recorded = Arc::clone(&calls);
        let callback = strict_callback(py, &["base"], &["repo_root", "owner"], move |py, args| {
            let base: String = args.get_item("base")?.extract()?;
            let owner: String = args.get_item("owner")?.extract()?;
            let root = args.get_item("repo_root")?.unbind();
            recorded.lock().unwrap().push((base, root, owner));
            Ok(py
                .import("builtins")?
                .getattr("set")?
                .call1((["mine.py"],))?
                .unbind())
        });
        let _patch = AttrPatch::replace(&subject, "changed_sources", callback.as_any());
        let narrow = scope_campaign(py, root, &["mine.py"], "fest");
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, root)).unwrap();
        kw.set_item("owner", "agent").unwrap();
        kw.set_item("base", "base-ref").unwrap();
        subject
            .getattr("validate_run_scope")
            .unwrap()
            .call((&narrow,), Some(&kw))
            .unwrap();
        let record = calls.lock().unwrap();
        assert_eq!(record.len(), 1);
        assert_eq!(record[0].0, "base-ref");
        equal(record[0].1.bind(py), &path(py, root));
        assert_eq!(record[0].2, "agent");
        drop(record);

        assert_run_cli_refuses_before_engine(py, root);
    });
}

#[test]
fn test_changed_test_admits_only_its_named_source() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = scope(py);
        let manifest = scope_campaign(py, case.root(), &["subject.py", "other.py"], "fest");
        let _patch = changed(py, &subject, &["tests/test_subject.py"]);
        let pins = PyDict::new(py);
        pins.set_item("tests/test_subject.py", "pin").unwrap();
        manifest.setattr("test_sha256", pins).unwrap();
        error(py, validate(py, &manifest, case.root(), None), "other.py");
        set_sources(py, &manifest, &["subject.py"]);
        equal(
            &validate(py, &manifest, case.root(), None).unwrap(),
            &PyList::new(py, ["subject.py"]).unwrap(),
        );
        manifest.setattr("test_sha256", PyDict::new(py)).unwrap();
        error(py, validate(py, &manifest, case.root(), None), "subject.py");
        let second = scope_campaign(py, case.root(), &["a/subject.py", "b/subject.py"], "fest");
        let pins = PyDict::new(py);
        pins.set_item("a/test_subject.py", "pin").unwrap();
        second.setattr("test_sha256", pins).unwrap();
        drop(_patch);
        let _patch = changed(py, &subject, &["a/test_subject.py"]);
        error(py, validate(py, &second, case.root(), None), "b/subject.py");
        set_sources(py, &second, &["a/subject.py"]);
        equal(
            &validate(py, &second, case.root(), None).unwrap(),
            &PyList::new(py, ["a/subject.py"]).unwrap(),
        );
    });
}

#[test]
fn test_rust_package_relative_path_is_checked_against_repo_relative_diff() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = scope(py);
        let manifest = scope_campaign(py, case.root(), &["crate/src/lib.rs"], "cargo-mutants");
        set_sources(py, &manifest, &["src/lib.rs"]);
        let options = PyDict::new(py);
        options.set_item("package_root", "crate").unwrap();
        manifest.setattr("options", options).unwrap();
        let patch = changed(py, &subject, &["crate/src/lib.rs"]);
        equal(
            &validate(py, &manifest, case.root(), None).unwrap(),
            &PyList::new(py, ["crate/src/lib.rs"]).unwrap(),
        );
        set_sources(py, &manifest, &["src/**/*.rs"]);
        error(
            py,
            validate(py, &manifest, case.root(), None),
            "exact repository files",
        );
        drop(patch);
        let second = scope_campaign(py, case.root(), &["src/lib.rs"], "cargo-mutants");
        let _patch = changed(py, &subject, &["src/lib.rs"]);
        equal(
            &validate(py, &second, case.root(), None).unwrap(),
            &PyList::new(py, ["src/lib.rs"]).unwrap(),
        );
    });
}

#[test]
fn test_symlink_escape_and_unpinned_source_refuse() {
    let case = Case::new();
    Python::attach(|py| {
        let root = case.mkdir("repo");
        let subject = scope(py);
        let manifest = scope_campaign(py, &root, &["mine.py"], "fest");
        fs::create_dir(root.join("nested")).unwrap();
        set_sources(py, &manifest, &["nested/../mine.py"]);
        error(
            py,
            validate(py, &manifest, &root, None),
            "exact repository files",
        );
        set_sources(py, &manifest, &["mine.py"]);
        let _patch = changed(py, &subject, &["mine.py"]);
        manifest.setattr("source_sha256", PyDict::new(py)).unwrap();
        error(py, validate(py, &manifest, &root, None), "pinned");
        set_sources(py, &manifest, &[]);
        error(py, validate(py, &manifest, &root, None), "pinned");
        let outside = case.write("outside.py", "x = 2\n");
        std::os::unix::fs::symlink(outside, root.join("link.py")).unwrap();
        set_sources(py, &manifest, &["link.py"]);
        error(
            py,
            validate(py, &manifest, &root, None),
            "outside the checkout",
        );
        let another = scope_campaign(py, &root, &["mine.py"], "fest");
        for bad in [
            "*.py",
            "?.py",
            "[a].py",
            "../escape.py",
            "/tmp/escape.py",
            "missing.py",
        ] {
            set_sources(py, &another, &[bad]);
            error(py, validate(py, &another, &root, None), "mutation scope");
        }
    });
}

#[test]
fn test_mull_config_filters_before_execution_and_escapes_regex() {
    let case = Case::new();
    Python::attach(|py| {
        let manifest = scope_campaign(py, case.root(), &["src/a+.cc"], "mull");
        manifest.setattr("operators", ("cxx_add_to_sub",)).unwrap();
        let config = scope(py)
            .getattr("mull_scope_config")
            .unwrap()
            .call1((&manifest, path(py, case.root())))
            .unwrap();
        let location: String = config.call_method0("__str__").unwrap().extract().unwrap();
        let lines: Vec<String> = fs::read_to_string(location)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines[0], "includePaths:");
        let pattern: String =
            serde_json::from_str(lines[1].trim().strip_prefix("- ").unwrap()).unwrap();
        let re = py.import("re").unwrap();
        let expected = case.root().join("src/a+.cc").to_string_lossy().into_owned();
        assert!(re
            .getattr("fullmatch")
            .unwrap()
            .call1((&pattern, expected))
            .unwrap()
            .is_truthy()
            .unwrap());
        for other in [
            case.root().join("src/aa.cc").to_string_lossy().into_owned(),
            case.root()
                .join("src/a+.cc.extra")
                .to_string_lossy()
                .into_owned(),
            format!("prefix{}", case.root().join("src/a+.cc").display()),
        ] {
            assert!(re
                .getattr("search")
                .unwrap()
                .call1((&pattern, other))
                .unwrap()
                .is_none());
        }
        assert_eq!(&lines[2..], &["mutators:", "  - \"cxx_add_to_sub\""]);
        manifest.setattr("operators", PyTuple::empty(py)).unwrap();
        let config = scope(py)
            .getattr("mull_scope_config")
            .unwrap()
            .call1((&manifest, path(py, case.root())))
            .unwrap();
        let body: String = config.call_method0("read_text").unwrap().extract().unwrap();
        assert!(!body.contains("mutators"));
        set_sources(py, &manifest, &[]);
        error(
            py,
            scope(py)
                .getattr("mull_scope_config")
                .unwrap()
                .call1((&manifest, path(py, case.root()))),
            "at least one",
        );
    });
}

#[test]
fn test_mull_globs_are_expanded_to_pins_before_ownership_check() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = scope(py);
        let manifest = scope_campaign(py, case.root(), &["src/one.cc", "src/two.cc"], "mull");
        set_sources(py, &manifest, &["src/**"]);
        let patch = changed(py, &subject, &["src/one.cc"]);
        error(py, validate(py, &manifest, case.root(), None), "src/two.cc");
        drop(patch);
        let _patch = changed(py, &subject, &["src/one.cc", "src/two.cc"]);
        equal(
            &validate(py, &manifest, case.root(), None).unwrap(),
            &PyList::new(py, ["src/one.cc", "src/two.cc"]).unwrap(),
        );
        let config = subject
            .getattr("mull_scope_config")
            .unwrap()
            .call1((&manifest, path(py, case.root())))
            .unwrap();
        let body: String = config.call_method0("read_text").unwrap().extract().unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 3);
        let pattern: String =
            serde_json::from_str(lines[1].trim().strip_prefix("- ").unwrap()).unwrap();
        let re = py.import("re").unwrap();
        let escaped: String = re
            .getattr("escape")
            .unwrap()
            .call1((case.root().join("src/one.cc").to_string_lossy().as_ref(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(pattern, format!("^{escaped}$"));
        set_sources(py, &manifest, &["missing/**"]);
        error(
            py,
            subject
                .getattr("mull_scope_config")
                .unwrap()
                .call1((&manifest, path(py, case.root()))),
            "no pinned target",
        );
    });
}
