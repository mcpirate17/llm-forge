#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the reuse audit inventory Python/native boundary.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/reuse_inventory_support.rs"]
mod inventory_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use inventory_support::{
    candidate, cluster, detectors, file_families, inventory, overrides, site, try_inventory,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::json;
use sha2::{Digest, Sha256};
use support::{assert_error, AttrPatch, Case};

fn category<'py>(result: &Bound<'py, PyDict>, name: &str) -> Bound<'py, PyList> {
    result
        .get_item(name)
        .unwrap()
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

#[test]
fn policy_thresholds_remain_python_owned() {
    let case = Case::new();
    Python::attach(|py| {
        let module = detectors(py);
        let file_limit = 10_i64.into_pyobject(py).unwrap();
        let function_limit = 5_i64.into_pyobject(py).unwrap();
        let _file = AttrPatch::replace(module.as_any(), "GOD_FILE_LINES", file_limit.as_any());
        let _function =
            AttrPatch::replace(module.as_any(), "GOD_FUNC_LINES", function_limit.as_any());
        let options = overrides(py);
        options
            .set_item(
                "god_files",
                PyList::new(
                    py,
                    [format!("{} (17)", case.root().join("large.py").display())],
                )
                .unwrap(),
            )
            .unwrap();
        options
            .set_item(
                "god_functions",
                PyList::new(
                    py,
                    [format!(
                        "{}:3 oversized (11)",
                        case.root().join("large.py").display()
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        let result = inventory(py, case.root(), &options);
        let file = category(&result, "god_files").get_item(0).unwrap();
        let function = category(&result, "god_functions").get_item(0).unwrap();
        assert!(file.get_item("value").unwrap().eq(7).unwrap());
        assert!(file
            .get_item("evidence")
            .unwrap()
            .eq("17 lines; threshold is 10")
            .unwrap());
        assert!(function.get_item("value").unwrap().eq(6).unwrap());
        let evidence: String = function.get_item("evidence").unwrap().extract().unwrap();
        assert!(evidence.ends_with("threshold is 5"));
    });
}

#[test]
fn clone_identity_is_order_independent_but_location_keeps_input_order() {
    let case = Case::new();
    Python::attach(|py| {
        let left = site(py, "pkg/z.py", 20, "z");
        let right = site(py, "pkg/a.py", 10, "a");
        let forward_options = overrides(py);
        forward_options
            .set_item(
                "clusters",
                PyList::new(
                    py,
                    [cluster(py, &PyList::new(py, [&left, &right]).unwrap())],
                )
                .unwrap(),
            )
            .unwrap();
        let reverse_options = overrides(py);
        reverse_options
            .set_item(
                "clusters",
                PyList::new(
                    py,
                    [cluster(py, &PyList::new(py, [&right, &left]).unwrap())],
                )
                .unwrap(),
            )
            .unwrap();
        let forward = category(&inventory(py, case.root(), &forward_options), "duplication")
            .get_item(0)
            .unwrap();
        let reverse = category(&inventory(py, case.root(), &reverse_options), "duplication")
            .get_item(0)
            .unwrap();
        let mut identities = ["pkg/z.py:20-24:z", "pkg/a.py:10-14:a"];
        identities.sort();
        let digest = format!("{:x}", Sha256::digest(identities.join("\0").as_bytes()));
        let expected_id = format!("reuse:{}", &digest[..20]);
        assert!(forward.get_item("id").unwrap().eq(&expected_id).unwrap());
        assert!(reverse.get_item("id").unwrap().eq(&expected_id).unwrap());
        assert!(forward
            .get_item("files")
            .unwrap()
            .eq(PyList::new(py, ["pkg/a.py", "pkg/z.py"]).unwrap())
            .unwrap());
        assert!(forward
            .get_item("location")
            .unwrap()
            .eq("pkg/z.py:20, pkg/a.py:10")
            .unwrap());
        assert!(reverse
            .get_item("location")
            .unwrap()
            .eq("pkg/a.py:10, pkg/z.py:20")
            .unwrap());
    });
}

struct RankingCandidates {
    fallbacks: Vec<Py<PyAny>>,
    native: Vec<Py<PyAny>>,
    tokens: Vec<Py<PyAny>>,
}

fn ranking_candidates(py: Python<'_>) -> RankingCandidates {
    let fallback_low = candidate(py, "fallback-low", 1, 0.1).into_any().unbind();
    let fallback_high = candidate(py, "fallback-high", 9, 0.9).into_any().unbind();
    let native_incomplete = candidate(py, "native-incomplete", 20, 0.9);
    native_incomplete
        .set_item("evidence_complete", false)
        .unwrap();
    let native_complete = candidate(py, "native-complete", 10, 0.8);
    native_complete.set_item("evidence_complete", 1).unwrap();
    let native_complete_low = candidate(py, "native-complete-low", 2, 0.7);
    native_complete_low
        .set_item("evidence_complete", "yes")
        .unwrap();
    let native_missing = candidate(py, "native-missing", 5, 1.0);
    let tokens = [
        candidate(py, "token-low", 1, 0.9).into_any().unbind(),
        candidate(py, "token-middle", 2, 0.1).into_any().unbind(),
        candidate(py, "token-high", 3, 0.5).into_any().unbind(),
    ];
    RankingCandidates {
        fallbacks: vec![fallback_low, fallback_high],
        native: vec![
            native_complete.into_any().unbind(),
            native_missing.into_any().unbind(),
            native_complete_low.into_any().unbind(),
            native_incomplete.into_any().unbind(),
        ],
        tokens: tokens.into(),
    }
}

#[test]
fn ranking_limits_and_pass_through_identity_match_python() {
    let case = Case::new();
    Python::attach(|py| {
        let RankingCandidates {
            fallbacks: fallback_values,
            native: native_values,
            tokens: token_values,
        } = ranking_candidates(py);
        let fallbacks = PyList::new(py, &fallback_values).unwrap();
        let native = PyList::new(py, &native_values).unwrap();
        let tokens = PyList::new(py, &token_values).unwrap();
        let options = overrides(py);
        options.set_item("fallbacks", &fallbacks).unwrap();
        options.set_item("token_clones", &tokens).unwrap();
        options.set_item("native_reuse", &native).unwrap();
        options.set_item("limit", -1).unwrap();
        let result = inventory(py, case.root(), &options);
        let duplication = category(&result, "duplication");
        assert!(duplication
            .eq(PyList::new(py, [&token_values[2], &token_values[1]]).unwrap())
            .unwrap());
        let silent = category(&result, "silent_fallbacks");
        assert!(silent
            .eq(PyList::new(py, [&fallback_values[1]]).unwrap())
            .unwrap());
        assert!(silent.get_item(0).unwrap().is(fallback_values[1].bind(py)));
        assert!(fallbacks
            .eq(PyList::new(py, [&fallback_values[1], &fallback_values[0]]).unwrap())
            .unwrap());
        let perf = category(&result, "perf_hotspots");
        assert!(perf
            .eq(PyList::new(py, [&native_values[0]]).unwrap())
            .unwrap());
        assert!(perf.get_item(0).unwrap().is(native_values[0].bind(py)));
        assert!(category(&result, "native_reuse")
            .eq(PyList::new(
                py,
                [&native_values[3], &native_values[0], &native_values[1]]
            )
            .unwrap())
            .unwrap());
        assert!(native
            .eq(PyList::new(
                py,
                [
                    &native_values[3],
                    &native_values[0],
                    &native_values[1],
                    &native_values[2]
                ]
            )
            .unwrap())
            .unwrap());
    });
}

#[test]
fn ties_duplicates_and_zero_limit_preserve_python_slice_contract() {
    let case = Case::new();
    Python::attach(|py| {
        let first = candidate(py, "first", 7, 0.5);
        first.set_item("extra", "preserved").unwrap();
        let second = candidate(py, "second", 7, 0.5);
        let winner = candidate(py, "confidence-winner", 7, 0.9);
        let duplicate = candidate(py, "duplicate", 3, 0.4);
        let ordered = PyList::new(
            py,
            [
                first.as_any(),
                second.as_any(),
                winner.as_any(),
                duplicate.as_any(),
                duplicate.as_any(),
            ],
        )
        .unwrap();
        let positive_options = overrides(py);
        positive_options.set_item("token_clones", &ordered).unwrap();
        positive_options.set_item("limit", 5).unwrap();
        let positive = category(
            &inventory(py, case.root(), &positive_options),
            "duplication",
        );
        let zero_options = overrides(py);
        zero_options.set_item("token_clones", &ordered).unwrap();
        zero_options.set_item("limit", 0).unwrap();
        let empty = category(&inventory(py, case.root(), &zero_options), "duplication");
        assert!(positive
            .eq(PyList::new(
                py,
                [
                    winner.as_any(),
                    first.as_any(),
                    second.as_any(),
                    duplicate.as_any(),
                    duplicate.as_any()
                ]
            )
            .unwrap())
            .unwrap());
        assert!(positive.get_item(0).unwrap().is(winner.as_any()));
        assert!(positive.get_item(1).unwrap().is(first.as_any()));
        assert!(positive.get_item(2).unwrap().is(second.as_any()));
        assert!(positive
            .get_item(3)
            .unwrap()
            .is(positive.get_item(4).unwrap()));
        assert!(positive.get_item(3).unwrap().is(duplicate.as_any()));
        assert!(empty.eq(PyList::empty(py)).unwrap());
    });
}

fn family(py: Python<'_>) -> Bound<'_, PyAny> {
    let options = py_json(
        py,
        json!({
            "files":["pkg/a.py","pkg/b.py"], "similarity_min":0.75,
            "similarity_avg":0.812, "similarity_max":0.9, "containment_min":0.8,
            "shared_methods":["shared"], "variable_methods":["vary"],
            "common_fields":["field"], "recommended_abstraction":"base-class",
            "suggested_home":null, "gross_duplicate_loc":400,
            "estimated_net_deleted_loc":220, "confidence":0.85,
            "risk":"medium", "band":"strong", "disposition":"validate",
            "before_loc":500, "after_loc":280, "target_shape":"base+config"
        }),
    );
    file_families(py)
        .getattr("FileFamily")
        .unwrap()
        .call((), Some(options.cast::<PyDict>().unwrap()))
        .unwrap()
}

#[test]
fn all_constructed_categories_keep_exact_output_contract() {
    let case = Case::new();
    Python::attach(|py| {
        let dependency = candidate(py, "dependency", 12, 0.6);
        let compliance = candidate(py, "compliance", 4, 0.5);
        let contract = candidate(py, "contract", 8, 0.7);
        let options = overrides(py);
        options
            .set_item("families", PyList::new(py, [family(py)]).unwrap())
            .unwrap();
        options
            .set_item(
                "vulture",
                PyList::new(
                    py,
                    [format!(
                        "{}:7: unused helper (75% confidence)",
                        case.root().join("dead.py").display()
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        options.set_item("ruff", py_json(py, json!([
            {"filename":case.root().join("lint.py"),"location":{"row":9},"code":"F401","message":"unused import","extra":"ignored"},
            {"filename":case.root().join("nullable.py"),"location":{"row":null},"code":null,"message":null}
        ]))).unwrap();
        options
            .set_item("dependencies", PyList::new(py, [&dependency]).unwrap())
            .unwrap();
        options
            .set_item("compliance", PyList::new(py, [&compliance]).unwrap())
            .unwrap();
        options
            .set_item("contract_candidates", PyList::new(py, [&contract]).unwrap())
            .unwrap();
        let result = inventory(py, case.root(), &options);
        let expected_dead = py_json(
            py,
            json!([{
                "id":"dead:dead.py:7", "category":"dead_code", "severity":"medium",
                "confidence":0.75,"value":30,"files":["dead.py"],
                "location":"dead.py:7","evidence":"unused helper"
            }]),
        );
        assert!(category(&result, "dead_code").eq(expected_dead).unwrap());
        let imports = category(&result, "imports_deps");
        assert!(imports
            .get_item(0)
            .unwrap()
            .get_item("id")
            .unwrap()
            .eq("dependency")
            .unwrap());
        assert!(imports.get_item(0).unwrap().is(dependency.as_any()));
        assert!(imports
            .get_item(1)
            .unwrap()
            .get_item("id")
            .unwrap()
            .eq("ruff:F401:lint.py:9")
            .unwrap());
        assert!(imports
            .get_item(2)
            .unwrap()
            .get_item("id")
            .unwrap()
            .eq("ruff:None:nullable.py:None")
            .unwrap());
        let digest = format!("{:x}", Sha256::digest(b"pkg/a.py\0pkg/b.py"));
        let expected_family = py_json(
            py,
            json!({
                "id":format!("family:{}", &digest[..20]),"category":"file_families",
                "severity":"high","confidence":0.85,"value":220,
                "files":["pkg/a.py","pkg/b.py"],"location":"pkg/a.py, pkg/b.py",
                "evidence":"strong family; min/avg similarity 75.0%/81.2%; containment>=80.0%; recommend=base-class; before_loc=500; after_loc=280; target_shape=base+config; net_deleted_loc=220; shared_methods=['shared']; variable_methods=['vary']; suggested_home=leader-selected",
                "before_loc":500,"after_loc":280,"target_shape":"base+config"
            }),
        );
        assert!(category(&result, "file_families")
            .get_item(0)
            .unwrap()
            .eq(expected_family)
            .unwrap());
        assert!(category(&result, "compliance")
            .eq(PyList::new(py, [&compliance]).unwrap())
            .unwrap());
        assert!(category(&result, "test_contracts")
            .eq(PyList::new(py, [&contract]).unwrap())
            .unwrap());
    });
}

#[test]
fn malformed_optional_measurements_are_skipped_but_required_ones_fail() {
    let case = Case::new();
    Python::attach(|py| {
        let options = overrides(py);
        options
            .set_item(
                "god_functions",
                PyList::new(py, ["not a function measurement"]).unwrap(),
            )
            .unwrap();
        options
            .set_item(
                "vulture",
                PyList::new(py, ["not a vulture measurement"]).unwrap(),
            )
            .unwrap();
        let result = inventory(py, case.root(), &options);
        assert!(category(&result, "god_functions")
            .eq(PyList::empty(py))
            .unwrap());
        assert!(category(&result, "dead_code")
            .eq(PyList::empty(py))
            .unwrap());
        let options = overrides(py);
        options
            .set_item(
                "god_files",
                PyList::new(py, ["not a file measurement"]).unwrap(),
            )
            .unwrap();
        let error = try_inventory(py, case.root(), &options).unwrap_err();
        assert_error(
            py,
            error,
            py.get_type::<PyValueError>().as_any(),
            "malformed god-file measurement",
        );
    });
}
