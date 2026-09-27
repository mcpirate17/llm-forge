#![cfg(feature = "python-compat-tests")]
//! Rust-owned re-export selection contracts for candidate review.

#[path = "python_contracts/graph_selection_support.rs"]
mod graph_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use graph_support::{context, snapshot, INIT, MODULE, SELECTED, SOURCE};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PySet};
use support::{module, path, Case};

fn selection<'py>(py: Python<'py>, case: &Case, source: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.graph_selection")
        .getattr("_convention_tests")
        .unwrap()
        .call1((context(py, case), vec![source]))
        .unwrap()
}

fn assert_selected(result: &Bound<'_, PyAny>, relative: &str) {
    let contains: bool = result
        .call_method1("__contains__", (relative,))
        .unwrap()
        .extract()
        .unwrap();
    assert!(contains, "expected selected test {relative}");
}

fn assert_empty_set(py: Python<'_>, result: &Bound<'_, PyAny>) {
    assert!(result.eq(PySet::empty(py).unwrap()).unwrap());
}

const IMPORTS_REEXPORT: &str = "from component_fab.equations import adapt_equation_distribution\ndef test_x():\n    assert adapt_equation_distribution()\n";

#[test]
fn reexported_import_selects_the_test() {
    let case = Case::new();
    snapshot(&case, INIT, MODULE, IMPORTS_REEXPORT);
    Python::attach(|py| assert_selected(&selection(py, &case, SOURCE), SELECTED));
}

#[test]
fn unrelated_package_import_is_not_selected() {
    let case = Case::new();
    snapshot(
        &case,
        INIT,
        MODULE,
        "from component_fab.equations import something_else\ndef test_x():\n    assert something_else\n",
    );
    Python::attach(|py| assert_empty_set(py, &selection(py, &case, SOURCE)));
}

#[test]
fn aliased_reexport_matches_the_alias() {
    let case = Case::new();
    snapshot(
        &case,
        "from .adaptation import adapt_equation_distribution as adapt_eq\n",
        MODULE,
        "from component_fab.equations import adapt_eq\ndef test_x():\n    assert adapt_eq()\n",
    );
    Python::attach(|py| assert_selected(&selection(py, &case, SOURCE), SELECTED));
}

#[test]
fn reexport_inside_try_except_is_found() {
    let case = Case::new();
    snapshot(
        &case,
        "try:\n    from .adaptation import adapt_equation_distribution\nexcept ImportError:\n    adapt_equation_distribution = None\n",
        MODULE,
        "from component_fab.equations import adapt_equation_distribution\ndef test_x():\n    assert adapt_equation_distribution\n",
    );
    Python::attach(|py| assert_selected(&selection(py, &case, SOURCE), SELECTED));
}

#[test]
fn star_reexport_uses_module_public_names() {
    let case = Case::new();
    snapshot(
        &case,
        "from .adaptation import *\n",
        MODULE,
        IMPORTS_REEXPORT,
    );
    Python::attach(|py| assert_selected(&selection(py, &case, SOURCE), SELECTED));
}

#[test]
fn star_reexport_respects_dunder_all() {
    let case = Case::new();
    snapshot(
        &case,
        "from .adaptation import *\n",
        "__all__ = [\"kept\"]\n\ndef kept():\n    return 1\n\ndef dropped():\n    return 2\n",
        "from component_fab.equations import dropped\ndef test_x():\n    assert dropped()\n",
    );
    Python::attach(|py| assert_empty_set(py, &selection(py, &case, SOURCE)));
}

#[test]
fn relative_import_in_test_does_not_match() {
    let case = Case::new();
    snapshot(
        &case,
        INIT,
        MODULE,
        "from .equations import adapt_equation_distribution\ndef test_x():\n    assert adapt_equation_distribution\n",
    );
    Python::attach(|py| assert_empty_set(py, &selection(py, &case, SOURCE)));
}

#[test]
fn filename_convention_still_selects() {
    let case = Case::new();
    case.write("snapshot/component_fab/foo.py", "x = 1\n");
    case.write(
        "snapshot/component_fab/tests/test_foo.py",
        "def test_x():\n    pass\n",
    );
    Python::attach(|py| {
        assert_selected(
            &selection(py, &case, "component_fab/foo.py"),
            "component_fab/tests/test_foo.py",
        );
    });
}

#[test]
fn dotted_module_string_still_selects() {
    let case = Case::new();
    case.write("snapshot/component_fab/foo.py", "x = 1\n");
    case.write(
        "snapshot/component_fab/tests/test_other.py",
        "import component_fab.foo\ndef test_x():\n    pass\n",
    );
    Python::attach(|py| {
        assert_selected(
            &selection(py, &case, "component_fab/foo.py"),
            "component_fab/tests/test_other.py",
        );
    });
}

#[test]
fn surfaces_skip_non_python_and_packageless_sources() {
    let case = Case::new();
    case.write(
        "snapshot/research/tools/loose.py",
        "def helper():\n    return 1\n",
    );
    Python::attach(|py| {
        let graph = module(py, "conductor.candidate_review.graph_selection");
        let surfaces = graph.getattr("_reexport_surfaces").unwrap();
        let ctx = context(py, &case);
        let loose = surfaces
            .call1((&ctx, vec!["research/tools/loose.py"]))
            .unwrap();
        assert!(loose.eq(PyDict::new(py)).unwrap());
        case.write("snapshot/component_fab/equations/config.json", "{}\n");
        case.write(
            "snapshot/component_fab/equations/__init__.py",
            "from .config import SETTINGS\n",
        );
        let non_python = surfaces
            .call1((&ctx, vec!["component_fab/equations/config.json"]))
            .unwrap();
        assert!(non_python.eq(PyDict::new(py)).unwrap());
    });
}

#[test]
fn public_names_excludes_underscored() {
    let case = Case::new();
    let source = case.write(
        "m.py",
        "def public():\n    return 1\n\ndef _private():\n    return 2\n\nclass Klass:\n    pass\n\nCONST = 3\n_HIDDEN = 4\n",
    );
    Python::attach(|py| {
        let names = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_public_names")
            .unwrap()
            .call1((path(py, &source),))
            .unwrap();
        assert!(names
            .eq(PySet::new(py, ["public", "Klass", "CONST"]).unwrap())
            .unwrap());
    });
}

#[test]
fn public_names_prefers_dunder_all() {
    let case = Case::new();
    let source = case.write(
        "m.py",
        "__all__ = [\"only\"]\n\ndef only():\n    return 1\n\ndef other():\n    return 2\n",
    );
    Python::attach(|py| {
        let names = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_public_names")
            .unwrap()
            .call1((path(py, &source),))
            .unwrap();
        assert!(names.eq(PySet::new(py, ["only"]).unwrap()).unwrap());
    });
}
