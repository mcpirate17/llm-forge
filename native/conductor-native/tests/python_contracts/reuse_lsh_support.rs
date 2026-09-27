//! Rust-owned seeded profiles and independent LSH reference for reuse contracts.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, AttrPatch};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyFrozenSet, PyList, PySet, PyTuple};
use std::collections::{BTreeMap, BTreeSet};

const PRIME: u128 = (1_u128 << 61) - 1;
const MAX_BUCKET_SIZE: usize = 80;

pub type Features = BTreeSet<String>;
pub type Pairs = BTreeSet<(usize, usize)>;

pub fn file_families(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.reuse.file_families")
}

pub fn profile<'py>(py: Python<'py>, index: usize, features: &Features) -> Bound<'py, PyAny> {
    let empty = PyFrozenSet::new(py, Vec::<String>::new()).unwrap();
    let options = PyDict::new(py);
    options
        .set_item("file", format!("pkg/profile_{index}.py"))
        .unwrap();
    options.set_item("loc", 100).unwrap();
    for key in [
        "classes",
        "function_names",
        "method_names",
        "api",
        "fields",
        "calls",
        "control",
        "imports",
    ] {
        options.set_item(key, &empty).unwrap();
    }
    options.set_item("method_hashes", PyDict::new(py)).unwrap();
    options
        .set_item(
            "structure",
            PyFrozenSet::new(py, features.iter().cloned()).unwrap(),
        )
        .unwrap();
    file_families(py)
        .getattr("FileProfile")
        .unwrap()
        .call((), Some(&options))
        .unwrap()
}

pub fn random_profiles(py: Python<'_>) -> (Vec<Features>, Bound<'_, PyList>) {
    let rng = module(py, "random")
        .getattr("Random")
        .unwrap()
        .call1((20260903,))
        .unwrap();
    let range = module(py, "builtins")
        .getattr("range")
        .unwrap()
        .call1((600,))
        .unwrap();
    let result = PyList::empty(py);
    let mut feature_rows = Vec::new();
    for index in 0..72 {
        let cluster = index / 12;
        let mut shared: Vec<String> = (0..28)
            .map(|slot| format!("cluster:{cluster}:{slot}"))
            .collect();
        shared.sort();
        let sample = rng
            .call_method1("sample", (PyList::new(py, shared).unwrap(), 19))
            .unwrap();
        let mut features: Features = sample
            .extract::<Vec<String>>()
            .unwrap()
            .into_iter()
            .collect();
        let noise = rng.call_method1("sample", (&range, 9)).unwrap();
        for slot in noise.extract::<Vec<usize>>().unwrap() {
            features.insert(format!("noise:{slot}"));
        }
        features.insert(format!("unique:{index}"));
        result.append(profile(py, index, &features)).unwrap();
        feature_rows.push(features);
    }
    (feature_rows, result)
}

fn feature_hash(py: Python<'_>, feature: &str) -> u128 {
    let options = PyDict::new(py);
    options.set_item("digest_size", 8).unwrap();
    let digest = module(py, "hashlib")
        .getattr("blake2b")
        .unwrap()
        .call((PyBytes::new(py, feature.as_bytes()),), Some(&options))
        .unwrap()
        .call_method0("digest")
        .unwrap()
        .extract::<Vec<u8>>()
        .unwrap();
    let bytes: [u8; 8] = digest.try_into().unwrap();
    u128::from(u64::from_be_bytes(bytes)) & PRIME
}

fn minhash_signature(py: Python<'_>, features: &Features, permutations: usize) -> Vec<u64> {
    let values: Vec<u128> = features
        .iter()
        .map(|feature| feature_hash(py, feature))
        .collect();
    (0..permutations)
        .map(|index| {
            let multiplier = (0x9E3779B185EBCA87_u128 + 2 * index as u128) % PRIME;
            let multiplier = multiplier.max(1);
            let offset = (0xC2B2AE3D27D4EB4F_u128 * (index as u128 + 1)) % PRIME;
            values
                .iter()
                .map(|value| ((multiplier * value + offset) % PRIME) as u64)
                .min()
                .unwrap()
        })
        .collect()
}

pub fn reference_pairs(
    py: Python<'_>,
    profiles: &[Features],
    permutations: usize,
    band_size: usize,
) -> Pairs {
    assert_eq!(permutations % band_size, 0);
    let mut buckets: BTreeMap<(usize, Vec<u64>), Vec<usize>> = BTreeMap::new();
    for (profile_index, features) in profiles.iter().enumerate() {
        let row = minhash_signature(py, features, permutations);
        for start in (0..permutations).step_by(band_size) {
            buckets
                .entry((start / band_size, row[start..start + band_size].to_vec()))
                .or_default()
                .push(profile_index);
        }
    }
    let mut pairs = Pairs::new();
    for members in buckets.values() {
        if !(2..=MAX_BUCKET_SIZE).contains(&members.len()) {
            continue;
        }
        for left in 0..members.len() {
            for right in left + 1..members.len() {
                pairs.insert((members[left], members[right]));
            }
        }
    }
    pairs
}

pub fn observed_pairs(value: &Bound<'_, PyAny>) -> Pairs {
    value
        .try_iter()
        .unwrap()
        .map(|pair| {
            let (left, right): (usize, usize) = pair.unwrap().extract().unwrap();
            (left.min(right), left.max(right))
        })
        .collect()
}

pub fn all_pairs(count: usize) -> Pairs {
    let mut pairs = Pairs::new();
    for left in 0..count {
        for right in left + 1..count {
            pairs.insert((left, right));
        }
    }
    pairs
}

fn bind_variadic<'py>(
    signature: &Py<PyAny>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
    required: &[&str],
) -> PyResult<Bound<'py, PyAny>> {
    let filtered = PyDict::new(args.py());
    if let Some(kwargs) = kwargs {
        for (key, value) in kwargs {
            let name: String = key.extract()?;
            if required.contains(&name.as_str()) {
                filtered.set_item(key, value)?;
            }
        }
    }
    bind_signature(signature, args, Some(&filtered))
}

pub fn patch_scan<'py>(
    py: Python<'py>,
    profiles: &Bound<'py, PyList>,
    exact: &Pairs,
) -> Vec<AttrPatch> {
    let module = file_families(py);
    let profiles = profiles.clone().into_any().unbind();
    let collect_sig = signature(py, &["repo", "targets", "exclude", "min_file_loc"], &[]);
    let collect =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&collect_sig, args, kwargs)?;
            let py = args.py();
            let count = 2_i64.into_pyobject(py)?;
            Ok(PyTuple::new(py, [profiles.bind(py), count.as_any()])?
                .into_any()
                .unbind())
        })
        .unwrap();
    let exact_pairs = PySet::empty(py).unwrap();
    for &(left, right) in exact {
        exact_pairs.add((left, right)).unwrap();
    }
    let exact_pairs = exact_pairs.into_any().unbind();
    let exact_sig = signature(py, &["profiles"], &[]);
    let exact_callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            // The original lambda accepted any exact-scoring keyword arguments.
            let bound = bind_variadic(&exact_sig, args, kwargs, &["profiles"])?;
            let py = args.py();
            let _ = bound;
            let count = 276_i64.into_pyobject(py)?;
            Ok(PyTuple::new(py, [exact_pairs.bind(py), count.as_any()])?
                .into_any()
                .unbind())
        })
        .unwrap();
    let build_sig = signature(py, &["profiles", "pairs"], &[]);
    let build =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_variadic(&build_sig, args, kwargs, &["profiles", "pairs"])?;
            let py = args.py();
            let empty = PyList::empty(py);
            let count = 4_i64.into_pyobject(py)?;
            Ok(PyTuple::new(py, [empty.as_any(), count.as_any()])?
                .into_any()
                .unbind())
        })
        .unwrap();
    vec![
        AttrPatch::replace(module.as_any(), "collect_profiles", collect.as_any()),
        AttrPatch::replace(
            module.as_any(),
            "exact_candidate_pairs",
            exact_callback.as_any(),
        ),
        AttrPatch::replace(module.as_any(), "build_families", build.as_any()),
    ]
}
