#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for exact file-family scoring and complete-link groups.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyFrozenSet, PyList, PySet, PyTuple};
use serde_json::{json, Value};
use support::{module, Case};

fn families(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.reuse.file_families")
}

fn frozen<'py>(
    py: Python<'py>,
    values: impl IntoIterator<Item = String>,
) -> Bound<'py, PyFrozenSet> {
    PyFrozenSet::new(py, values).unwrap()
}

fn profiles(py: Python<'_>, seed: i64) -> Bound<'_, PyList> {
    let rng = module(py, "random")
        .getattr("Random")
        .unwrap()
        .call1((seed,))
        .unwrap();
    let population = module(py, "builtins")
        .getattr("range")
        .unwrap()
        .call1((100,))
        .unwrap();
    let result = PyList::empty(py);
    for index in 0..15 {
        let cluster = index / 5;
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("file", format!("pkg/family_{cluster}/member_{index}.py"))
            .unwrap();
        kwargs.set_item("loc", 120 + index).unwrap();
        kwargs
            .set_item("classes", frozen(py, ["Lane".to_owned()]))
            .unwrap();
        kwargs
            .set_item("function_names", frozen(py, ["run".to_owned()]))
            .unwrap();
        kwargs
            .set_item("method_names", frozen(py, ["forward".to_owned()]))
            .unwrap();
        let hashes = PyDict::new(py);
        hashes
            .set_item("forward", frozen(py, [format!("hash:{cluster}")]))
            .unwrap();
        kwargs.set_item("method_hashes", hashes).unwrap();
        kwargs.set_item("schemas", frozen(py, Vec::new())).unwrap();
        for component in ["structure", "api", "fields", "calls", "control", "imports"] {
            let mut values: Vec<String> = (0..32)
                .map(|slot| format!("{component}:cluster:{cluster}:{slot}"))
                .collect();
            let sample = rng.call_method1("sample", (&population, 5)).unwrap();
            for slot in sample.try_iter().unwrap() {
                let slot: i64 = slot.unwrap().extract().unwrap();
                values.push(format!("{component}:noise:{slot}"));
            }
            kwargs.set_item(component, frozen(py, values)).unwrap();
        }
        result
            .append(
                families(py)
                    .getattr("FileProfile")
                    .unwrap()
                    .call((), Some(&kwargs))
                    .unwrap(),
            )
            .unwrap();
    }
    result
}

fn pairs<'py>(py: Python<'py>) -> Bound<'py, PySet> {
    let expected = PySet::empty(py).unwrap();
    for left in 0..15 {
        for right in left + 1..15 {
            expected.add((left, right)).unwrap();
        }
    }
    expected
}

fn kwargs<'py>(py: Python<'py>, similarity: f64) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("min_similarity", similarity).unwrap();
    kwargs.set_item("min_shared_features", 12).unwrap();
    kwargs
}

fn attr<T: for<'a> FromPyObject<'a, 'a>>(item: &Bound<'_, PyAny>, name: &str) -> T {
    item.getattr(name).unwrap().extract().ok().unwrap()
}

#[test]
fn native_pair_scoring_and_safe_bound_match_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let profiles = profiles(py, 20260901);
        let native = families(py);
        let representative = native
            .getattr("compare_profiles")
            .unwrap()
            .call1((profiles.get_item(2).unwrap(), profiles.get_item(3).unwrap()))
            .unwrap();
        assert_eq!(attr::<f64>(&representative, "score"), 0.7769);
        assert_eq!(attr::<f64>(&representative, "containment"), 0.8703);
        assert_eq!(attr::<i64>(&representative, "shared_features"), 161);
        let expected = py_json(
            py,
            json!({
                "structure":0.8049,"api":0.7619,"fields":0.7619,"calls":0.7619,
                "control":0.7619,"imports":0.7619
            }),
        );
        assert!(representative
            .getattr("components")
            .unwrap()
            .eq(expected)
            .unwrap());
        let expected_pairs = pairs(py);
        for threshold in [0.55, 0.70, 0.90] {
            let actual = native
                .getattr("exact_candidate_pairs")
                .unwrap()
                .call((profiles.clone(),), Some(&kwargs(py, threshold)))
                .unwrap();
            let expected = PyTuple::new(
                py,
                [
                    expected_pairs.as_any(),
                    105_i64.into_pyobject(py).unwrap().as_any(),
                ],
            )
            .unwrap();
            assert!(actual.eq(expected).unwrap());
        }
    });
}

fn family_summary(family: &Bound<'_, PyAny>) -> Value {
    let files = family.getattr("files").unwrap();
    let files: Vec<String> = files.cast::<PyList>().unwrap().extract().unwrap();
    json!([
        attr::<String>(family, "id"),
        files,
        attr::<f64>(family, "similarity_min"),
        attr::<f64>(family, "similarity_avg"),
        attr::<f64>(family, "similarity_max"),
        attr::<f64>(family, "containment_min"),
        attr::<i64>(family, "gross_duplicate_loc"),
        attr::<i64>(family, "estimated_net_deleted_loc"),
        attr::<i64>(family, "before_loc"),
        attr::<i64>(family, "after_loc")
    ])
}

#[test]
fn native_complete_link_groups_and_ids_match_python() {
    let _case = Case::new();
    Python::attach(|py| {
        let profiles = profiles(py, 17);
        let native = families(py);
        let exact = native
            .getattr("exact_candidate_pairs")
            .unwrap()
            .call((profiles.clone(),), Some(&kwargs(py, 0.70)))
            .unwrap();
        let family_kwargs = kwargs(py, 0.70);
        family_kwargs.set_item("min_net_deleted_loc", 0).unwrap();
        family_kwargs.set_item("max_family_size", 8).unwrap();
        family_kwargs.set_item("max_candidates", 50).unwrap();
        let actual = native
            .getattr("build_families")
            .unwrap()
            .call((profiles, exact.get_item(0).unwrap()), Some(&family_kwargs))
            .unwrap();
        assert!(actual.get_item(1).unwrap().eq(105).unwrap());
        let families = actual.get_item(0).unwrap();
        let summary: Vec<Value> = families
            .try_iter()
            .unwrap()
            .map(|item| family_summary(&item.unwrap()))
            .collect();
        assert_eq!(
            Value::Array(summary),
            json!([
                [
                    "F001",
                    [
                        "pkg/family_2/member_10.py",
                        "pkg/family_2/member_11.py",
                        "pkg/family_2/member_12.py",
                        "pkg/family_2/member_13.py",
                        "pkg/family_2/member_14.py"
                    ],
                    0.7619,
                    0.7733,
                    0.7855,
                    0.8649,
                    408,
                    381,
                    660,
                    279
                ],
                [
                    "F002",
                    [
                        "pkg/family_1/member_5.py",
                        "pkg/family_1/member_6.py",
                        "pkg/family_1/member_7.py",
                        "pkg/family_1/member_8.py",
                        "pkg/family_1/member_9.py"
                    ],
                    0.7619,
                    0.7688,
                    0.7814,
                    0.8649,
                    392,
                    366,
                    635,
                    269
                ],
                [
                    "F003",
                    [
                        "pkg/family_0/member_0.py",
                        "pkg/family_0/member_1.py",
                        "pkg/family_0/member_2.py",
                        "pkg/family_0/member_3.py",
                        "pkg/family_0/member_4.py"
                    ],
                    0.7619,
                    0.7716,
                    0.7837,
                    0.8649,
                    376,
                    350,
                    610,
                    260
                ]
            ])
        );
    });
}
