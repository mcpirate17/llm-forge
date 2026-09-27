#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the native file-family LSH boundary.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/reuse_lsh_support.rs"]
mod lsh_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use lsh_support::{
    all_pairs, file_families, observed_pairs, patch_scan, profile, random_profiles,
    reference_pairs, Features,
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySet};
use serde_json::json;
use std::path::Path;
use support::{assert_error, path, Case};

fn pairs<'py>(
    py: Python<'py>,
    profiles: &Bound<'py, PyList>,
    permutations: i64,
    band_size: i64,
) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
    let options = PyDict::new(py);
    options.set_item("permutations", permutations)?;
    options.set_item("band_size", band_size)?;
    file_families(py)
        .getattr("candidate_pairs")?
        .call((profiles,), Some(&options))
}

fn expect_error<T>(py: Python<'_>, result: PyResult<T>, class: &str, message: &str) {
    let error = match result {
        Ok(_) => panic!("expected {class}"),
        Err(error) => error,
    };
    let exception = py.import("builtins").unwrap().getattr(class).unwrap();
    assert_error(py, error, &exception, message);
}

#[test]
fn native_lsh_pairs_match_independent_reference() {
    let _case = Case::new();
    Python::attach(|py| {
        let (features, profiles) = random_profiles(py);
        let hash = file_families(py)
            .getattr("_feature_hash")
            .unwrap()
            .call1(("mask-contract",))
            .unwrap();
        assert!(hash.eq(1_601_420_257_758_367_900_u64).unwrap());
        for (permutations, band_size) in [(12, 3), (24, 4), (48, 4), (48, 8)] {
            let expected = reference_pairs(py, &features, permutations, band_size);
            let observed = pairs(py, &profiles, permutations as i64, band_size as i64).unwrap();
            assert_eq!(
                observed_pairs(&observed),
                expected,
                "{permutations}/{band_size}"
            );
        }
    });
}

#[test]
fn native_lsh_pairs_preserve_bucket_and_argument_boundaries() {
    let _case = Case::new();
    Python::attach(|py| {
        let identical: Features = ["shared:a", "shared:b", "shared:c", "shared:d"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let profiles_80 =
            PyList::new(py, (0..80).map(|index| profile(py, index, &identical))).unwrap();
        let profiles_81 =
            PyList::new(py, (0..81).map(|index| profile(py, index, &identical))).unwrap();
        assert_eq!(
            observed_pairs(&pairs(py, &profiles_80, 8, 2).unwrap()),
            all_pairs(80)
        );
        assert!(observed_pairs(&pairs(py, &profiles_81, 8, 2).unwrap()).is_empty());

        let empty = PyList::new(
            py,
            [profile(py, 0, &Features::new()), profile(py, 1, &identical)],
        )
        .unwrap();
        expect_error(
            py,
            pairs(py, &empty, 8, 2),
            "ValueError",
            "min() arg is an empty sequence",
        );
        assert!(observed_pairs(&pairs(py, &empty, 0, 2).unwrap()).is_empty());
        let negative =
            PyList::new(py, (0..4).map(|index| profile(py, index, &Features::new()))).unwrap();
        assert_eq!(
            observed_pairs(&pairs(py, &negative, -4, -2).unwrap()),
            all_pairs(4)
        );
        expect_error(
            py,
            pairs(py, &profiles_80, 7, 2),
            "ValueError",
            "num_permutations",
        );
        expect_error(py, pairs(py, &profiles_80, 8, 0), "ZeroDivisionError", "");
    });
}

#[test]
fn scan_file_families_reports_native_lsh_telemetry() {
    let _case = Case::new();
    Python::attach(|py| {
        let (features, profiles) = random_profiles(py);
        let first: Vec<Features> = features.into_iter().take(24).collect();
        let first_profiles = PyList::new(py, profiles.iter().take(24).collect::<Vec<_>>()).unwrap();
        let lsh_pairs = reference_pairs(py, &first, 12, 3);
        let exact: lsh_support::Pairs = lsh_pairs.iter().take(4).copied().collect();
        assert_eq!(exact.len(), 4);
        let _patches = patch_scan(py, &first_profiles, &exact);
        let options = PyDict::new(py);
        options.set_item("permutations", 12).unwrap();
        options.set_item("band_size", 3).unwrap();
        let targets = PyList::new(py, ["pkg"]).unwrap();
        let exclude = PySet::empty(py).unwrap();
        let output = file_families(py)
            .getattr("scan_file_families")
            .unwrap()
            .call((path(py, Path::new(".")), targets, exclude), Some(&options))
            .unwrap();
        let families = output.get_item(0).unwrap();
        assert!(families.cast::<PyList>().unwrap().is_empty());
        let recall =
            ((lsh_pairs.intersection(&exact).count() as f64 / 4.0) * 10_000.0).round() / 10_000.0;
        let expected = py_json(
            py,
            json!({
                "files_profiled":24, "files_unparsable":2, "pair_universe":276,
                "exact_candidate_pairs":4, "pairs_pruned_by_safe_bound":272,
                "lsh_candidate_pairs":lsh_pairs.len(), "lsh_recall":recall,
                "pairs_scored":4, "families":0, "estimated_net_deleted_loc":0
            }),
        );
        assert!(output.get_item(1).unwrap().eq(expected).unwrap());
    });
}
