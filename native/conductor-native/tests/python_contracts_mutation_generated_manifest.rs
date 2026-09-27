#![cfg(feature = "python-compat-tests")]
//! Generated campaign manifest and adapter selection contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_generated_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use fixture::{campaign_error, generated, load, manifest};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use serde_json::json;
use support::{module, path, Case};

#[test]
fn a_hand_written_campaign_is_refused_by_this_runner() {
    let case = Case::new();
    let file = manifest(&case, json!({"mutation_engine":"reviewed_unified_diff"}));
    Python::attach(|py| {
        let runner = generated(py);
        assert!(runner
            .getattr("manifest_engine")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap()
            .eq("reviewed_unified_diff")
            .unwrap());
        campaign_error(
            py,
            runner
                .getattr("load_generated_campaign")
                .unwrap()
                .call1((path(py, &file),)),
            "is not a generated engine",
        );
        campaign_error(
            py,
            runner
                .getattr("adapter_for")
                .unwrap()
                .call1(("reviewed_unified_diff",)),
            "is not a generated engine",
        );
    });
}

#[test]
fn every_declared_engine_resolves_to_an_adapter() {
    let _case = Case::new();
    Python::attach(|py| {
        let callable = module(py, "builtins").getattr("callable").unwrap();
        for engine in ["fest", "cargo-mutants"] {
            let adapter = generated(py)
                .getattr("adapter_for")
                .unwrap()
                .call1((engine,))
                .unwrap();
            assert!(adapter.getattr("ENGINE").unwrap().eq(engine).unwrap());
            for attr in ["binary", "execute"] {
                assert!(callable
                    .call1((adapter.getattr(attr).unwrap(),))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap());
            }
        }
    });
}

#[test]
fn a_manifest_missing_a_required_key_is_refused() {
    let case = Case::new();
    let file = case.write("bad.json", "{\"campaign_id\": \"x\"}");
    Python::attach(|py| {
        campaign_error(
            py,
            generated(py)
                .getattr("load_generated_campaign")
                .unwrap()
                .call1((path(py, &file),)),
            "missing required key 'title'",
        );
    });
}

#[test]
fn the_test_command_is_pinned_to_this_interpreter() {
    let _case = Case::new();
    Python::attach(|py| {
        let pinned = generated(py).getattr("pinned").unwrap();
        for command in ["python", "python3"] {
            let actual = pinned
                .call1((vec![command, "-m", "pytest"], "/v/bin/python"))
                .unwrap()
                .cast_into::<PyList>()
                .unwrap();
            assert!(actual
                .eq(PyList::new(py, ["/v/bin/python", "-m", "pytest"]).unwrap())
                .unwrap());
        }
        let cargo = pinned
            .call1((vec!["cargo", "test"], "/v/bin/python"))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert!(cargo
            .eq(PyList::new(py, ["cargo", "test"]).unwrap())
            .unwrap());
        let empty = pinned
            .call1((PyList::empty(py), "/v/bin/python"))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert!(empty.eq(PyList::empty(py)).unwrap());
    });
}

#[test]
fn the_source_globs_must_name_something() {
    let case = Case::new();
    let file = manifest(
        &case,
        json!({"generator":{"source":[],"run_timeout_seconds":60}}),
    );
    Python::attach(|py| {
        campaign_error(
            py,
            generated(py)
                .getattr("load_generated_campaign")
                .unwrap()
                .call1((path(py, &file),)),
            "at least one glob",
        );
    });
}

#[test]
fn the_campaign_pins_the_bytes_it_measured() {
    let case = Case::new();
    let file = manifest(&case, json!({"source_sha256":{}}));
    Python::attach(|py| {
        campaign_error(
            py,
            generated(py)
                .getattr("load_generated_campaign")
                .unwrap()
                .call1((path(py, &file),)),
            "must pin every mutated file",
        );
    });
}

#[test]
fn the_optional_generator_keys_reach_the_campaign() {
    let case = Case::new();
    let file = manifest(
        &case,
        json!({"generator":{
            "source":["conductor/gate_rollout.py"], "exclude":["**/test_*.py"],
            "operators":["constant_*"], "options":{"package_root":"native"},
            "seed":7, "jobs":4, "mutant_timeout_seconds":45, "run_timeout_seconds":60
        }}),
    );
    Python::attach(|py| {
        let campaign = load(py, &file);
        assert!(campaign
            .getattr("exclude")
            .unwrap()
            .eq(PyTuple::new(py, ["**/test_*.py"]).unwrap())
            .unwrap());
        assert!(campaign
            .getattr("operators")
            .unwrap()
            .eq(PyTuple::new(py, ["constant_*"]).unwrap())
            .unwrap());
        assert!(campaign
            .getattr("options")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap()
            .eq(py_json(py, json!({"package_root":"native"})))
            .unwrap());
        assert_eq!(
            campaign.getattr("seed").unwrap().extract::<i32>().unwrap(),
            7
        );
        assert_eq!(
            campaign.getattr("jobs").unwrap().extract::<i32>().unwrap(),
            4
        );
        assert_eq!(
            campaign
                .getattr("mutant_timeout_seconds")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            45
        );
    });
}

#[test]
fn the_environment_a_manifest_declares_reaches_the_run() {
    let case = Case::new();
    let file = manifest(&case, json!({"environment":{"CARGO_TERM_COLOR":"never"}}));
    Python::attach(|py| {
        let campaign = load(py, &file);
        assert!(campaign
            .getattr("environment")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap()
            .eq(py_json(py, json!({"CARGO_TERM_COLOR":"never"})))
            .unwrap());
    });
}

#[test]
fn the_defaults_are_the_ones_the_manifests_were_written_against() {
    let case = Case::new();
    let file = manifest(&case, json!({}));
    Python::attach(|py| {
        let campaign = load(py, &file);
        assert!(campaign
            .getattr("exclude")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        assert!(campaign
            .getattr("operators")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        assert!(campaign
            .getattr("options")
            .unwrap()
            .eq(PyDict::new(py))
            .unwrap());
        assert_eq!(
            campaign.getattr("seed").unwrap().extract::<i32>().unwrap(),
            0
        );
        assert_eq!(
            campaign.getattr("jobs").unwrap().extract::<i32>().unwrap(),
            1
        );
        assert!(campaign
            .getattr("mutant_timeout_seconds")
            .unwrap()
            .is_none());
    });
}
