#![cfg(feature = "python-compat-tests")]
//! Elements report normalization, scope, and pinned fixture contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_mull_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{
    campaign, campaign_error, default_report, equal, field, generated, manifest, mull, mutant,
    mutant_with, py_json, repo_src, report, rows, SOURCE, SOURCE_TEXT,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use support::{module, path, Case};

fn row<'py>(rows: &Bound<'py, PyList>, index: usize) -> Bound<'py, PyAny> {
    rows.get_item(index).unwrap()
}
fn string(value: &Bound<'_, PyAny>) -> String {
    value.extract().unwrap()
}
fn merged<'py>(py: Python<'py>, reports: Vec<Value>) -> Bound<'py, PyAny> {
    mull(py)
        .getattr("_merge")
        .unwrap()
        .call1((py_json(py, json!(reports)),))
        .unwrap()
}

#[test]
fn every_status_mull_can_report_is_mapped_and_none_is_guessed() {
    let _case = Case::new();
    Python::attach(|py| {
        let statuses = [
            ("Killed", 2, 12),
            ("Survived", 2, 20),
            ("NoCoverage", 3, 10),
            ("Timeout", 3, 12),
            ("CompileError", 4, 3),
            ("Ignored", 4, 4),
            ("RuntimeError", 5, 1),
        ];
        let mutants = statuses.iter().map(|(s, l, c)| mutant(s, *l, *c)).collect();
        let actual = rows(py, default_report(mutants));
        let outcomes: Vec<_> = actual
            .iter()
            .map(|r| string(&field(&r, "outcome")))
            .collect();
        assert_eq!(
            outcomes,
            [
                "KILLED",
                "SURVIVED",
                "NO_COVERAGE",
                "TIMED_OUT",
                "UNVIABLE",
                "UNVIABLE",
                "ERROR"
            ]
        );
        campaign_error(
            py,
            mull(py)
                .getattr("_rows")
                .unwrap()
                .call1((
                    py_json(py, default_report(vec![mutant("Flaky", 2, 12)])),
                    path(py, &repo_src()),
                ))
                .unwrap_err(),
            "unknown status",
        );
    });
}

fn location(py: Python<'_>, sl: i64, sc: i64, el: i64, ec: i64) -> Bound<'_, PyAny> {
    py_json(
        py,
        json!({"start":{"line":sl,"column":sc},"end":{"line":el,"column":ec}}),
    )
}

#[test]
fn the_original_text_is_sliced_out_of_the_source_the_report_carries() {
    let _case = Case::new();
    Python::attach(|py| {
        let slice = mull(py).getattr("_slice").unwrap();
        for (source, coords, expected) in [
            (SOURCE_TEXT, (2, 21, 2, 22), "<"),
            (SOURCE_TEXT, (3, 9, 3, 10), "*"),
            ("abcd\nefgh\nijkl", (1, 2, 3, 3), "bcd\nefgh\nij"),
            (SOURCE_TEXT, (99, 1, 99, 2), ""),
            ("abc\ndef", (0, 1, 0, 2), ""),
        ] {
            assert!(slice
                .call1((source, location(py, coords.0, coords.1, coords.2, coords.3)))
                .unwrap()
                .eq(expected)
                .unwrap());
        }
        let multi = string(
            &slice
                .call1((SOURCE_TEXT, location(py, 2, 3, 4, 4)))
                .unwrap(),
        );
        assert!(multi.starts_with("for (int i"));
        assert!(multi.ends_with('}'));
        assert!(multi.contains("g(i * 2);"));
        let mut missing = default_report(vec![mutant_with(
            "cxx_lt_to_le",
            "<=",
            "Survived",
            1,
            1,
            None,
            2,
            None,
        )]);
        let key = repo_src().join(SOURCE).to_string_lossy().into_owned();
        missing["files"][&key]
            .as_object_mut()
            .unwrap()
            .remove("source");
        assert!(field(&row(&rows(py, missing.clone()), 0), "original_text")
            .eq("")
            .unwrap());
        let merge = merged(py, vec![missing.clone()]);
        assert!(merge
            .get_item("files")
            .unwrap()
            .get_item(&key)
            .unwrap()
            .get_item("source")
            .unwrap()
            .eq("")
            .unwrap());
        let merge = merged(py, vec![default_report(vec![mutant("Survived", 2, 12)])]);
        assert!(merge
            .get_item("files")
            .unwrap()
            .get_item(&key)
            .unwrap()
            .get_item("source")
            .unwrap()
            .eq(SOURCE_TEXT)
            .unwrap());
        missing["files"][&key]["mutants"][0]
            .as_object_mut()
            .unwrap()
            .remove("status");
        campaign_error(
            py,
            mull(py)
                .getattr("_rows")
                .unwrap()
                .call1((py_json(py, missing), path(py, &repo_src())))
                .unwrap_err(),
            "unknown status ''",
        );
    });
}

#[test]
fn a_mutant_is_named_independently_of_the_line_it_sits_on() {
    let _case = Case::new();
    Python::attach(|py| {
        let early = rows(
            py,
            default_report(vec![mutant_with(
                "cxx_lt_to_le",
                "<=",
                "Survived",
                2,
                21,
                None,
                22,
                None,
            )]),
        );
        let moved = format!("\n\n\n{SOURCE_TEXT}");
        let late = rows(
            py,
            report(
                vec![mutant_with(
                    "cxx_lt_to_le",
                    "<=",
                    "Survived",
                    5,
                    21,
                    None,
                    22,
                    None,
                )],
                SOURCE,
                &moved,
            ),
        );
        equal(&field(&row(&early, 0), "id"), &field(&row(&late, 0), "id"));
        assert!(field(&row(&early, 0), "line").eq(2).unwrap());
        assert!(field(&row(&late, 0), "line").eq(5).unwrap());
    });
}

#[test]
fn repeats_of_one_mutation_in_a_file_are_separate_mutants() {
    let _case = Case::new();
    Python::attach(|py| {
        let actual = rows(
            py,
            default_report(vec![
                mutant_with("cxx_lt_to_le", "<=", "Survived", 3, 21, None, 22, None),
                mutant_with("cxx_lt_to_le", "<=", "Survived", 2, 21, None, 22, None),
            ]),
        );
        let lines: Vec<i64> = actual
            .iter()
            .map(|r| field(&r, "line").extract().unwrap())
            .collect();
        assert_eq!(lines, [2, 3]);
        let ids: HashSet<String> = actual.iter().map(|r| string(&field(&r, "id"))).collect();
        assert_eq!(ids.len(), 2);
    });
}

#[test]
fn every_receipt_field_a_row_carries_is_pinned() {
    let _case = Case::new();
    Python::attach(|py| {
        let actual = rows(
            py,
            default_report(vec![mutant_with(
                "cxx_lt_to_le",
                "<=",
                "Survived",
                2,
                21,
                None,
                22,
                None,
            )]),
        );
        assert_eq!(actual.len(), 1);
        let row = row(&actual, 0);
        let keys = module(py, "builtins")
            .getattr("set")
            .unwrap()
            .call1((row.call_method0("keys").unwrap(),))
            .unwrap();
        let expected = pyo3::types::PySet::new(
            py,
            [
                "id",
                "outcome",
                "path",
                "line",
                "operator",
                "original_text",
                "mutated_text",
            ],
        )
        .unwrap();
        equal(&keys, &expected);
        for (key, value) in [
            ("path", SOURCE),
            ("operator", "cxx_lt_to_le"),
            ("original_text", "<"),
            ("mutated_text", "<="),
        ] {
            assert!(field(&row, key).eq(value).unwrap());
        }
        assert!(field(&row, "line").eq(2).unwrap());
    });
}

#[test]
fn a_mutant_outside_the_worktree_stops_the_run() {
    let _case = Case::new();
    Python::attach(|py| {
        let outside = json!({"files":{"/usr/include/c++/13/cmath":{"source":SOURCE_TEXT,"mutants":[mutant("Survived",2,12)]}}});
        campaign_error(
            py,
            mull(py)
                .getattr("_rows")
                .unwrap()
                .call1((py_json(py, outside), path(py, &repo_src())))
                .unwrap_err(),
            "outside the worktree",
        );
    });
}

#[test]
fn a_kill_by_either_suite_outranks_a_survival_in_the_other() {
    let _case = Case::new();
    Python::attach(|py| {
        let one = |status| {
            default_report(vec![mutant_with(
                "cxx_lt_to_le",
                "<=",
                status,
                2,
                12,
                None,
                13,
                Some("m1"),
            )])
        };
        for pair in [
            [one("Killed"), one("Survived")],
            [one("Survived"), one("Killed")],
        ] {
            let actual = mull(py)
                .getattr("_rows")
                .unwrap()
                .call1((merged(py, pair.to_vec()), path(py, &repo_src())))
                .unwrap()
                .cast_into::<PyList>()
                .unwrap();
            assert_eq!(actual.len(), 1);
            assert!(field(&row(&actual, 0), "outcome").eq("KILLED").unwrap());
        }
        let actual = mull(py)
            .getattr("_rows")
            .unwrap()
            .call1((
                merged(py, vec![one("NoCoverage"), one("Survived")]),
                path(py, &repo_src()),
            ))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert_eq!(actual.len(), 1);
        assert!(field(&row(&actual, 0), "outcome").eq("SURVIVED").unwrap());
    });
}

#[test]
fn the_manifests_source_globs_scope_a_corpus_the_tool_cannot_scope() {
    let _case = Case::new();
    Python::attach(|py| {
        let scope = campaign(py).getattr("source").unwrap();
        let expected = PyTuple::new(py, ["aria_core/src/cpu/**"]).unwrap();
        equal(&scope, &expected);
        let api = mull(py).getattr("_in_scope").unwrap();
        for (source, yes) in [
            ("aria_core/src/cpu/math_space.cpp", true),
            ("aria_core/src/cpu/simd_elementwise.h", true),
            ("aria_designer/runtime/tests/test_kernels.cpp", false),
        ] {
            assert_eq!(
                api.call1((source, &scope))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                yes
            );
        }
    });
}

#[test]
fn a_recursive_glob_reaches_a_subdirectory_that_does_not_exist_yet() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = mull(py).getattr("_in_scope").unwrap();
        for (source, scope, yes) in [
            (
                "aria_core/src/cpu/simd/avx512.cpp",
                "aria_core/src/cpu/**",
                true,
            ),
            (
                "aria_core/src/cpu/a/b/c/deep.cpp",
                "aria_core/src/cpu/**",
                true,
            ),
            ("aria_core/src/gpu/kernel.cu", "aria_core/src/cpu/**", false),
            ("aria_core/src/cpu/x.cpp", "aria_core/src/cpu/*.cpp", true),
            (
                "aria_core/src/cpu/sub/x.cpp",
                "aria_core/src/cpu/*.cpp",
                false,
            ),
        ] {
            let one = PyTuple::new(py, [scope]).unwrap();
            assert_eq!(
                api.call1((source, one)).unwrap().extract::<bool>().unwrap(),
                yes
            );
        }
    });
}

#[test]
fn the_campaign_under_test_is_wired_end_to_end() {
    let _case = Case::new();
    Python::attach(|py| {
        let paths = module(py, "conductor.project_paths");
        let host = paths
            .getattr("host_root")
            .unwrap()
            .call1((path(py, &repo_src()),))
            .unwrap();
        let registry = paths
            .getattr("registry_path")
            .unwrap()
            .call1((&host,))
            .unwrap();
        let read_kw = PyDict::new(py);
        read_kw.set_item("encoding", "utf-8").unwrap();
        let text: String = registry
            .call_method("read_text", (), Some(&read_kw))
            .unwrap()
            .extract()
            .unwrap();
        let data: Value = serde_json::from_str(&text).unwrap();
        let entries = data["campaigns"].as_array().unwrap();
        let mut selected = Vec::new();
        for entry in entries {
            if let Some(relative) = entry.get("manifest").and_then(Value::as_str) {
                let manifest = host.call_method1("__truediv__", (relative,)).unwrap();
                if generated(py)
                    .getattr("manifest_engine")
                    .unwrap()
                    .call1((&manifest,))
                    .unwrap()
                    .eq("mull")
                    .unwrap()
                {
                    selected.push(manifest);
                }
            }
        }
        if selected.is_empty() {
            return;
        } // Matches the original explicit pytest skip: no live Mull campaign.
        for manifest in selected {
            let loaded = generated(py)
                .getattr("load_generated_campaign")
                .unwrap()
                .call1((&manifest,))
                .unwrap();
            assert!(loaded
                .getattr("mutation_engine")
                .unwrap()
                .eq("mull")
                .unwrap());
            assert!(loaded.getattr("language").unwrap().eq("cpp").unwrap());
            assert!(loaded.getattr("jobs").unwrap().eq(1).unwrap());
            assert!(loaded
                .getattr("survivor_baseline")
                .unwrap()
                .is_truthy()
                .unwrap());
            assert!(mull(py)
                .getattr("_executables")
                .unwrap()
                .call1((&loaded,))
                .unwrap()
                .is_truthy()
                .unwrap());
            let hashes = loaded.getattr("source_sha256").unwrap();
            for relative in hashes.call_method0("keys").unwrap().try_iter().unwrap() {
                assert!(host
                    .call_method1("__truediv__", (relative.unwrap(),))
                    .unwrap()
                    .call_method0("is_file")
                    .unwrap()
                    .extract::<bool>()
                    .unwrap());
            }
        }
    });
}

#[test]
fn the_recorded_baseline_is_the_corrected_measurement_not_the_broken_one() {
    let _case = Case::new();
    Python::attach(|py| {
        let data: Value = serde_json::from_str(&fs::read_to_string(manifest()).unwrap()).unwrap();
        assert_eq!(data["survivor_baseline"].as_array().unwrap().len(), 202);
        let argv = mull(py)
            .getattr("_engine_argv")
            .unwrap()
            .call1((
                campaign(py),
                "/bin/mull-runner-18",
                path(py, std::path::Path::new("/b/t")),
                path(py, std::path::Path::new("/b/t.profdata")),
                path(py, std::path::Path::new("/b/r")),
                "t",
            ))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        let values: Vec<String> = argv.extract().unwrap();
        assert!(values.contains(&"--coverage-info".to_owned()));
    });
}

#[test]
fn the_committed_campaign_makes_uninitialised_reads_deterministic() {
    let _case = Case::new();
    Python::attach(|py| {
        let environment = campaign(py).getattr("environment").unwrap();
        assert!(environment
            .call_method1("get", ("MALLOC_PERTURB_",))
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}
