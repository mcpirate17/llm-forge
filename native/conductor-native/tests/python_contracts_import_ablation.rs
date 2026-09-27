#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for module-level import ablation and its two oracles.

#[path = "python_contracts/import_ablation_support.rs"]
mod ablation_support;
#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use ablation_support::{classify_patches, collection_site, ia, sample, simple_site, sites};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::fs;
use std::process::Command;
use support::{path, Case};

#[test]
fn only_silenced_imports_are_candidates() {
    let case = Case::new();
    let source = sample(&case);
    Python::attach(|py| {
        let silenced: Vec<String> = sites(py, &source, false)
            .iter()
            .map(|site| site.getattr("statement").unwrap().extract().unwrap())
            .collect();
        let every: Vec<String> = sites(py, &source, true)
            .iter()
            .map(|site| site.getattr("statement").unwrap().extract().unwrap())
            .collect();
        assert!(silenced
            .iter()
            .any(|statement| statement.contains("import os")));
        assert!(!silenced.iter().any(|statement| statement == "import sys"));
        assert!(every.iter().any(|statement| statement == "import sys"));
    });
}

#[test]
fn multiline_import_is_removed_whole() {
    let case = Case::new();
    let source = sample(&case);
    Python::attach(|py| {
        let site = collection_site(py, &source);
        let text = fs::read_to_string(&source).unwrap();
        let ablated: String = ia(py)
            .getattr("ablate_source")
            .unwrap()
            .call1((text, site))
            .unwrap()
            .extract()
            .unwrap();
        assert!(!ablated.contains("OrderedDict"));
        assert!(!ablated.contains("defaultdict"));
        py.import("builtins")
            .unwrap()
            .getattr("compile")
            .unwrap()
            .call1((&ablated, "<ablated>", "exec"))
            .unwrap();
    });
}

#[test]
fn ablation_never_writes_to_the_module() {
    let case = Case::new();
    let source = sample(&case);
    let before = fs::read(&source).unwrap();
    Python::attach(|py| {
        let site = sites(py, &source, false).get_item(0).unwrap();
        let text = fs::read_to_string(&source).unwrap();
        ia(py)
            .getattr("ablate_source")
            .unwrap()
            .call1((text, site))
            .unwrap();
    });
    assert_eq!(fs::read(&source).unwrap(), before);
}

#[test]
fn finder_serves_ablated_source_for_one_module_only() {
    let case = Case::new();
    let source = sample(&case);
    Python::attach(|py| {
        let site = collection_site(py, &source);
        let text = fs::read_to_string(&source).unwrap();
        let ablated = ia(py)
            .getattr("ablate_source")
            .unwrap()
            .call1((text, site))
            .unwrap();
        let finder = ia(py)
            .getattr("AblatedFinder")
            .unwrap()
            .call1(("sample", path(py, &source), ablated))
            .unwrap();
        assert!(!finder
            .call_method1("find_spec", ("sample",))
            .unwrap()
            .is_none());
        assert!(finder
            .call_method1("find_spec", ("something_else",))
            .unwrap()
            .is_none());
    });
}

fn run_git(case: &Case, args: &[&str]) {
    let result = Command::new("git")
        .args(args)
        .current_dir(case.root())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn consumers_see_parenthesized_multiline_import() {
    let case = Case::new();
    run_git(&case, &["init", "-q"]);
    fs::create_dir(case.root().join("pkg")).unwrap();
    fs::write(case.root().join("pkg/leaf.py"), "NAME = 1\n").unwrap();
    fs::write(
        case.root().join("pkg/user.py"),
        "from pkg.leaf import (\n    NAME,\n)\n",
    )
    .unwrap();
    run_git(&case, &["add", "-A"]);
    Python::attach(|py| {
        let names = PyList::new(py, ["NAME"]).unwrap();
        let found = ia(py)
            .getattr("consumers")
            .unwrap()
            .call1(("pkg/leaf.py", names, path(py, case.root())))
            .unwrap();
        assert!(found.contains("pkg/user.py").unwrap());
    });
}

#[test]
fn no_driver_tests_is_reported_not_clean() {
    let case = Case::new();
    Python::attach(|py| {
        let site = simple_site(py);
        let record = ia(py)
            .getattr("classify")
            .unwrap()
            .call1((site, "m.py", PyList::empty(py), path(py, case.root())))
            .unwrap();
        assert!(record
            .get_item("verdict")
            .unwrap()
            .eq(ia(py).getattr("NOT_EXERCISED").unwrap())
            .unwrap());
    });
}

fn classify_with_mock(returncode: i32, found: &[&str], verdict: &str) {
    let case = Case::new();
    fs::write(case.root().join("m.py"), "import os  # noqa: F401\n").unwrap();
    Python::attach(|py| {
        let _patches = classify_patches(py, returncode, found);
        let site = simple_site(py);
        let drivers = PyList::new(py, ["test_m.py"]).unwrap();
        let record = ia(py)
            .getattr("classify")
            .unwrap()
            .call1((site, "m.py", drivers, path(py, case.root())))
            .unwrap();
        assert!(record
            .get_item("verdict")
            .unwrap()
            .eq(ia(py).getattr(verdict).unwrap())
            .unwrap());
    });
}

#[test]
fn failing_drivers_mean_the_import_is_load_bearing() {
    classify_with_mock(1, &["other.py"], "LOAD_BEARING");
}

#[test]
fn passing_drivers_with_no_consumers_is_dead_candidate() {
    classify_with_mock(0, &[], "UNVERIFIED");
}
