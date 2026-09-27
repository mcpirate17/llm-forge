#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for the native ablation engine's Python surface.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::{PyImportError, PyKeyError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use std::collections::BTreeSet;
use support::{assert_error, module, AttrPatch, Case};

const SAMPLE: &str = include_str!("../../../src/conductor/testdata/native_ablations/sample.py");

fn na(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.native_ablations")
}

fn ablations<'py>(py: Python<'py>, source: &str) -> Bound<'py, PyList> {
    na(py)
        .getattr("ablations")
        .unwrap()
        .call1((source,))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

fn set_of(items: &Bound<'_, PyAny>) -> BTreeSet<String> {
    items
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().extract::<String>().unwrap())
        .collect()
}

fn rules(items: &Bound<'_, PyList>) -> BTreeSet<String> {
    items
        .iter()
        .map(|item| item.getattr("rule").unwrap().extract().unwrap())
        .collect()
}

#[test]
fn rules_split_into_default_and_measured_optout() {
    let _case = Case::new();
    Python::attach(|py| {
        let names = na(py).getattr("rule_names").unwrap().call0().unwrap();
        assert_eq!(names.len().unwrap(), 2);
        let defaults = names.get_item(0).unwrap();
        let optional = names.get_item(1).unwrap();
        assert!(defaults.len().unwrap() > 15);
        let base = set_of(&defaults);
        let extras = set_of(&optional);
        assert!(extras.contains("drop_contiguous"));
        assert!(extras.contains("ablate_function_to_passthrough"));
        assert!(base.is_disjoint(&extras));
    });
}

#[test]
fn every_ablation_compiles() {
    let _case = Case::new();
    Python::attach(|py| {
        let compile = module(py, "builtins").getattr("compile").unwrap();
        for ablation in ablations(py, SAMPLE).iter() {
            let changed = ablation.call_method1("apply", (SAMPLE,)).unwrap();
            compile.call1((changed, "<mutant>", "exec")).unwrap();
        }
    });
}

#[test]
fn ablation_changes_the_source_it_names() {
    let _case = Case::new();
    Python::attach(|py| {
        for ablation in ablations(py, SAMPLE).iter() {
            let changed: String = ablation
                .call_method1("apply", (SAMPLE,))
                .unwrap()
                .extract()
                .unwrap();
            let rule: String = ablation.getattr("rule").unwrap().extract().unwrap();
            let line: i64 = ablation.getattr("line").unwrap().extract().unwrap();
            assert_ne!(changed, SAMPLE, "{rule} at line {line} changed nothing");
            assert!(line >= 1);
        }
    });
}

#[test]
fn unknown_rule_is_refused_not_ignored() {
    let _case = Case::new();
    Python::attach(|py| {
        let ablations = na(py).getattr("ablations").unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("rules", PyList::new(py, ["nonexistent_rule"]).unwrap())
            .unwrap();
        let error = ablations.call((SAMPLE,), Some(&kwargs)).unwrap_err();
        assert_error(
            py,
            error,
            py.get_type::<PyKeyError>().as_any(),
            "nonexistent_rule",
        );
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("extra", PyList::new(py, ["also_not_a_rule"]).unwrap())
            .unwrap();
        let error = ablations.call((SAMPLE,), Some(&kwargs)).unwrap_err();
        assert!(error.matches(py, &py.get_type::<PyKeyError>()).unwrap());
    });
}

#[test]
fn optional_rules_are_off_until_asked_for() {
    let _case = Case::new();
    Python::attach(|py| {
        let base = rules(&ablations(py, SAMPLE));
        assert!(!base.contains("ablate_function_to_passthrough"));
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("extra", vec!["ablate_function_to_passthrough"])
            .unwrap();
        let widened = na(py)
            .getattr("ablations")
            .unwrap()
            .call((SAMPLE,), Some(&kwargs))
            .unwrap();
        assert!(rules(widened.cast::<PyList>().unwrap()).contains("ablate_function_to_passthrough"));
    });
}

#[test]
fn qualname_and_new_rule_families_are_reachable() {
    let _case = Case::new();
    Python::attach(|py| {
        let found = ablations(py, SAMPLE);
        let names = rules(&found);
        for name in [
            "drop_import",
            "pin_parameter_to_default",
            "flip_boolean_default",
            "ablate_function_to_none",
        ] {
            assert!(names.contains(name), "missing rule {name}");
        }
        assert!(found
            .iter()
            .any(|a| a.getattr("qualname").unwrap().eq("helper").unwrap()));
    });
}

#[test]
fn multi_site_edits_apply_together() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = "def f(x, scale=2.0):\n    a = x * scale\n    return a + scale\n";
        let found = ablations(py, source);
        let pins: Vec<_> = found
            .iter()
            .filter(|a| {
                a.getattr("rule")
                    .unwrap()
                    .eq("pin_parameter_to_default")
                    .unwrap()
            })
            .collect();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].getattr("edits").unwrap().len().unwrap(), 2);
        let changed: String = pins[0]
            .call_method1("apply", (source,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(changed.contains("x * 2.0"));
        assert!(changed.contains("a + 2.0"));
    });
}

struct DictPatch(Py<PyAny>);

impl DictPatch {
    fn missing_slop_core(py: Python<'_>) -> Self {
        let modules = module(py, "sys").getattr("modules").unwrap();
        let missing = PyDict::new(py);
        missing.set_item("slop_core", py.None()).unwrap();
        let patcher = module(py, "unittest.mock")
            .getattr("patch")
            .unwrap()
            .getattr("dict")
            .unwrap()
            .call1((modules, missing))
            .unwrap();
        patcher.call_method0("__enter__").unwrap();
        Self(patcher.unbind())
    }
}

impl Drop for DictPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
        });
    }
}

#[test]
fn a_missing_engine_is_one_named_import_time_decision() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        let _missing = DictPatch::missing_slop_core(py);
        let result = native
            .getattr("_import_slop_core")
            .unwrap()
            .call0()
            .unwrap();
        assert!(result.get_item(0).unwrap().is_none());
        let reason = result.get_item(1).unwrap();
        assert!(!reason.is_none());
        let reason: String = reason.extract().unwrap();
        assert!(reason.contains("slop_core is not installed"));
        assert!(reason.contains("make slop-core"));
        let _engine = AttrPatch::replace(native.as_any(), "_SLOP_CORE", py.None().bind(py));
        let value = reason.into_pyobject(py).unwrap();
        let _why = AttrPatch::replace(native.as_any(), "SLOP_CORE_UNAVAILABLE", value.as_any());
        let error = native.getattr("slop_core").unwrap().call0().unwrap_err();
        assert!(error.matches(py, &py.get_type::<PyImportError>()).unwrap());
        assert_error(
            py,
            error,
            native.getattr("SlopCoreUnavailable").unwrap().as_any(),
            "make slop-core",
        );
    });
}

#[test]
fn installed_engine_is_handed_out_once_and_unchanged() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        assert!(native.getattr("SLOP_CORE_UNAVAILABLE").unwrap().is_none());
        let engine = native.getattr("slop_core").unwrap().call0().unwrap();
        let expected = na(py).getattr("_core").unwrap();
        assert!(engine.is(&expected));
    });
}
