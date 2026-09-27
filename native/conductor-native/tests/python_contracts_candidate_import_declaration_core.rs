#![cfg(feature = "python-compat-tests")]
//! Direct import-declaration, manifest, and tree-scope contracts in Rust.

#[path = "python_contracts/candidate_import_support.rs"]
#[allow(dead_code)]
mod import_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use import_support::{equal, import_decl, py_frozenset, py_json, py_tuple, MANIFEST};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use serde_json::json;
use std::fs;
use support::{module, path, Case};

fn call<'py>(py: Python<'py>, name: &str, arg: impl IntoPyObject<'py>) -> Bound<'py, PyAny> {
    import_decl(py)
        .getattr(name)
        .unwrap()
        .call1((arg,))
        .unwrap()
}

fn pairs<'py>(py: Python<'py>, rows: &[(&str, &[&str])]) -> Bound<'py, PyAny> {
    let rows: Vec<_> = rows
        .iter()
        .map(|(name, providers)| {
            PyTuple::new(
                py,
                [
                    (*name).into_pyobject(py).unwrap().into_any(),
                    PyTuple::new(py, *providers).unwrap().into_any(),
                ],
            )
            .unwrap()
            .into_any()
        })
        .collect();
    PyTuple::new(py, rows).unwrap().into_any()
}

fn imports<'py>(
    py: Python<'py>,
    name: &str,
    source: &str,
    declared: &[&str],
    distributions: serde_json::Value,
    stdlib: &[&str],
    base: Option<&[&str]>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("declared", PyTuple::new(py, declared).unwrap())
        .unwrap();
    kwargs
        .set_item("distributions", py_json(py, distributions))
        .unwrap();
    kwargs
        .set_item("stdlib", PyTuple::new(py, stdlib).unwrap())
        .unwrap();
    if let Some(base) = base {
        kwargs
            .set_item("base", PyTuple::new(py, base).unwrap())
            .unwrap();
    }
    import_decl(py)
        .getattr(name)
        .unwrap()
        .call((source,), Some(&kwargs))
        .unwrap()
}

fn tree(py: Python<'_>, rel: &str, trees: Option<&Bound<'_, PyAny>>) -> bool {
    let api = import_decl(py).getattr("in_base_dependency_tree").unwrap();
    match trees {
        Some(trees) => api.call1((rel, trees)).unwrap().is_truthy().unwrap(),
        None => api.call1((rel,)).unwrap().is_truthy().unwrap(),
    }
}

#[test]
fn a_spelling_difference_is_not_an_undeclared_dependency() {
    let _case = Case::new();
    Python::attach(|py| {
        let first = call(py, "canonical_name", "Ruamel_YAML");
        let second = call(py, "canonical_name", "ruamel.yaml");
        equal(&first, &second);
        assert_eq!(
            call(py, "canonical_name", "ruamel-yaml")
                .extract::<String>()
                .unwrap(),
            "ruamel-yaml"
        );
    });
}

#[test]
fn a_requirement_is_matched_without_its_version_or_extras() {
    let _case = Case::new();
    Python::attach(|py| {
        for (spec, expected) in [
            ("a2a-sdk[http-server]>=1.1.2", "a2a-sdk"),
            ("torch>=2.2", "torch"),
            ("orjson; python_version >= '3.12'", "orjson"),
            ("slop-core @ file:///tooling/native", "slop-core"),
        ] {
            assert_eq!(
                call(py, "requirement_name", spec)
                    .extract::<String>()
                    .unwrap(),
                expected
            );
        }
    });
}

#[test]
fn an_extra_or_group_declares_a_dependency() {
    let _case = Case::new();
    Python::attach(|py| {
        let declared = call(py, "declared_distributions", MANIFEST);
        let expected = py_frozenset(py, &["polars", "scipy", "pytest", "hatchling", "probe"]);
        assert!(expected
            .call_method1("issubset", (&declared,))
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}

#[test]
fn the_nearest_manifest_governs_a_file() {
    let case = Case::new();
    fs::write(case.root().join("pyproject.toml"), MANIFEST).unwrap();
    let inner = case.root().join("tooling/native");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join("pyproject.toml"), MANIFEST).unwrap();
    Python::attach(|py| {
        let result = import_decl(py)
            .getattr("nearest_manifest")
            .unwrap()
            .call1((path(py, case.root()), "tooling/native/src/mod.py"))
            .unwrap();
        assert!(result
            .is_instance(&module(py, "pathlib").getattr("Path").unwrap())
            .unwrap());
        equal(&result, &path(py, &inner.join("pyproject.toml")));
    });
}

#[test]
fn a_file_under_no_manifest_has_nothing_to_declare() {
    let case = Case::new();
    Python::attach(|py| {
        assert!(import_decl(py)
            .getattr("nearest_manifest")
            .unwrap()
            .call1((path(py, case.root()), "scratch/tool.py"))
            .unwrap()
            .is_none())
    });
}

#[test]
fn only_absolute_top_level_imports_carry_a_distribution() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = "import polars.selectors\nfrom . import sibling\nfrom scipy.stats import t\n";
        equal(
            &call(py, "imported_modules", source),
            &py_frozenset(py, &["polars", "scipy"]),
        );
    });
}

#[test]
fn an_import_that_resolves_to_nothing_is_out_of_scope() {
    let _case = Case::new();
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("declared", PyTuple::empty(py)).unwrap();
        kwargs
            .set_item("distributions", py_json(py, json!({})))
            .unwrap();
        let result = import_decl(py)
            .getattr("undeclared_imports")
            .unwrap()
            .call(("import conductor\nimport hydra\n",), Some(&kwargs))
            .unwrap();
        equal(&result, &py_tuple(py, &[]));
    });
}

#[test]
fn the_standard_library_is_never_a_dependency() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = imports(
            py,
            "undeclared_imports",
            "import json\nimport tomllib\n",
            &[],
            json!({"json":["json"]}),
            &["json", "tomllib"],
            None,
        );
        equal(&result, &py_tuple(py, &[]));
    });
}

#[test]
fn an_undeclared_installed_distribution_is_reported() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = imports(
            py,
            "undeclared_imports",
            "import yaml\n",
            &["polars"],
            json!({"yaml":["PyYAML"]}),
            &[],
            None,
        );
        equal(&result, &pairs(py, &[("yaml", &["PyYAML"])]));
    });
}

#[test]
fn a_declared_distribution_passes_however_it_is_spelled() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = imports(
            py,
            "undeclared_imports",
            "import yaml\n",
            &["pyyaml"],
            json!({"yaml":["PyYAML"]}),
            &[],
            None,
        );
        equal(&result, &py_tuple(py, &[]));
    });
}

#[test]
fn base_distributions_excludes_everything_optional() {
    let _case = Case::new();
    Python::attach(|py| {
        equal(
            &call(py, "base_distributions", MANIFEST),
            &py_frozenset(py, &["polars", "probe"]),
        )
    });
}

#[test]
fn an_extra_only_import_is_reported_in_a_base_dependency_tree() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = imports(
            py,
            "optional_only_imports",
            "import scipy\n",
            &["polars", "scipy"],
            json!({"scipy":["scipy"]}),
            &[],
            Some(&["polars"]),
        );
        equal(&result, &pairs(py, &[("scipy", &["scipy"])]));
    });
}

#[test]
fn an_entirely_undeclared_import_is_not_reported_twice() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = imports(
            py,
            "optional_only_imports",
            "import yaml\n",
            &["polars"],
            json!({"yaml":["PyYAML"]}),
            &[],
            Some(&["polars"]),
        );
        equal(&result, &py_tuple(py, &[]));
    });
}

#[test]
fn a_type_checking_import_does_not_execute() {
    let _case = Case::new();
    Python::attach(|py| {
        let source="from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    import scipy\nelse:\n    import polars\nimport yaml\n";
        equal(
            &call(py, "imported_modules", source),
            &py_frozenset(py, &["typing", "scipy", "polars", "yaml"]),
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("runtime_only", true).unwrap();
        let runtime = import_decl(py)
            .getattr("imported_modules")
            .unwrap()
            .call((source,), Some(&kwargs))
            .unwrap();
        equal(&runtime, &py_frozenset(py, &["typing", "polars", "yaml"]));
    });
}

#[test]
fn the_qualified_type_checking_spelling_is_also_a_guard() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = "import typing\nif typing.TYPE_CHECKING:\n    import scipy\n";
        let kwargs = PyDict::new(py);
        kwargs.set_item("runtime_only", true).unwrap();
        let runtime = import_decl(py)
            .getattr("imported_modules")
            .unwrap()
            .call((source,), Some(&kwargs))
            .unwrap();
        equal(&runtime, &py_frozenset(py, &["typing"]));
    });
}

#[test]
fn the_strict_rule_governs_the_trees_that_run_on_base_dependencies() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(tree(py, "conductor/radon_complexity.py", None));
        assert!(tree(py, "tooling/hooks/dispatch/runner.py", None));
        assert!(!tree(py, "conductor/conftest.py", None));
    });
}

#[test]
fn base_dependency_trees_default_to_the_monorepo_pair() {
    let case = Case::new();
    Python::attach(|py| {
        let result = call(py, "base_dependency_trees", path(py, case.root()));
        equal(&result, &py_tuple(py, &["conductor/", "tooling/"]));
        equal(
            &call(py, "base_dependency_trees", path(py, case.root())),
            &import_decl(py).getattr("BASE_DEPENDENCY_TREES").unwrap(),
        );
    });
}

fn src_layout(case: &Case) {
    fs::write(
        case.root().join("pyproject.toml"),
        "[tool.conductor]\npackage_root = \"src/conductor\"\n",
    )
    .unwrap();
}

#[test]
fn base_dependency_trees_follow_a_declared_src_layout() {
    let case = Case::new();
    src_layout(&case);
    Python::attach(|py| {
        equal(
            &call(py, "base_dependency_trees", path(py, case.root())),
            &py_tuple(py, &["src/conductor/", "src/tooling/"]),
        )
    });
}

#[test]
fn the_stricter_rule_governs_the_package_under_a_src_layout() {
    let case = Case::new();
    src_layout(&case);
    Python::attach(|py| {
        let trees = call(py, "base_dependency_trees", path(py, case.root()));
        assert!(tree(py, "src/conductor/radon_complexity.py", Some(&trees)));
        assert!(tree(
            py,
            "src/tooling/hooks/dispatch/runner.py",
            Some(&trees)
        ));
        assert!(!tree(py, "src/conductor/radon_complexity.py", None));
    });
}

#[test]
fn test_infrastructure_is_excluded_from_any_tree() {
    let case = Case::new();
    src_layout(&case);
    Python::attach(|py| {
        let trees = call(py, "base_dependency_trees", path(py, case.root()));
        assert!(!tree(py, "src/conductor/conftest.py", Some(&trees)));
    });
}

#[test]
fn a_tree_outside_the_pair_is_not_governed() {
    let case = Case::new();
    Python::attach(|py| {
        let trees = call(py, "base_dependency_trees", path(py, case.root()));
        assert!(!tree(py, "research/model.py", Some(&trees)));
        assert!(!tree(py, "native/build.py", Some(&trees)));
    });
}

#[test]
fn an_unparseable_root_manifest_leaves_the_trees_at_their_default() {
    let case = Case::new();
    fs::write(case.root().join("pyproject.toml"), "[project\nname =").unwrap();
    Python::attach(|py| {
        let err = import_decl(py)
            .getattr("base_dependency_trees")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        let kind = module(py, "tomllib").getattr("TOMLDecodeError").unwrap();
        assert!(err.matches(py, &kind).unwrap());
    });
}
