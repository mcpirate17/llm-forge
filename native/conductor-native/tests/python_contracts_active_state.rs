#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for active-state generation, validation, and durable writes.

#[path = "python_contracts/active_state_session_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::strict_callback;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use std::fs;
use support::{assert_error, module, path, AttrPatch, Case};

fn empty_list_callback(py: Python<'_>) -> Bound<'_, PyAny> {
    strict_callback(py, &["_repo"], &[], |bound| {
        Ok(PyList::empty(bound.py()).into_any().unbind())
    })
}

fn empty_claim_store(py: Python<'_>) -> Bound<'_, PyAny> {
    strict_callback(py, &["_root"], &[], |bound| {
        let result = PyTuple::new(
            bound.py(),
            [
                PyTuple::empty(bound.py()).into_any(),
                "empty-claim-store".into_pyobject(bound.py())?.into_any(),
            ],
        )?;
        Ok(result.into_any().unbind())
    })
}

fn active_claim<'py>(
    py: Python<'py>,
    claim_id: &str,
    expires_at: &Bound<'py, PyAny>,
) -> Bound<'py, PyDict> {
    let claim = PyDict::new(py);
    claim.set_item("claim_id", claim_id).unwrap();
    claim.set_item("expires_at", expires_at).unwrap();
    claim
}

#[test]
fn top_headings_skip_the_document_title_and_stop_at_limit() {
    let case = Case::new();
    let work = case.write(
        ".current_work.md",
        "# Active Coordination\n## ✅ Task 1: Finished\nDetails here...\n## ➡️ Task 2: In Progress\nMore details...\n## 🛑 Task 3: Blocked\n",
    );
    Python::attach(|py| {
        let active = module(py, "conductor.active_state");
        let _global_path =
            AttrPatch::replace(active.as_any(), "CURRENT_WORK_PATH", &path(py, &work));
        let options = PyDict::new(py);
        options.set_item("limit", 2).unwrap();
        let headings = active
            .getattr("parse_top_headings")
            .unwrap()
            .call((), Some(&options))
            .unwrap();
        assert_eq!(headings.len().unwrap(), 2);
        let first: String = headings.get_item(0).unwrap().extract().unwrap();
        let second: String = headings.get_item(1).unwrap().extract().unwrap();
        assert!(first.contains("Task 1"));
        assert!(second.contains("Task 2"));
    });
}

#[test]
fn generated_state_is_saved_as_valid_json_without_temporary_files() {
    let case = Case::new();
    let target = case.root().join("active_state.json");
    Python::attach(|py| {
        let active = module(py, "conductor.active_state");
        let ownership = module(py, "conductor.candidate_review.ownership");
        let _claims = AttrPatch::replace(
            ownership.as_any(),
            "load_claims",
            empty_claim_store(py).as_any(),
        );
        let _default = AttrPatch::replace(active.as_any(), "ACTIVE_STATE_PATH", &path(py, &target));
        let state = active
            .getattr("save_active_state")
            .unwrap()
            .call1((path(py, &target),))
            .unwrap();
        assert!(target.is_file());
        let payload: Bound<'_, PyAny> = PyModule::import(py, "json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((fs::read_to_string(&target).unwrap(),))
            .unwrap();
        assert_eq!(
            payload
                .get_item("schema_version")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert!(payload
            .get_item("standing_mandates")
            .unwrap()
            .eq(PyList::empty(py))
            .unwrap());
        assert_eq!(
            state
                .getattr("schema_version")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        let leftovers = fs::read_dir(case.root())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".active_state.json.")
                    && entry.file_name().to_string_lossy().ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    });
}

#[test]
fn generation_uses_standing_mandates_from_the_selected_repository() {
    let case = Case::new();
    case.write(
        "repo/pyproject.toml",
        "[tool.conductor.session]\npreamble = [\"PROJECT: policy\"]\nstanding_mandates = [\"PROJECT_RULE: required\"]\n",
    );
    Python::attach(|py| {
        let active = module(py, "conductor.active_state");
        let _claims = AttrPatch::replace(
            active.as_any(),
            "parse_active_claims",
            empty_list_callback(py).as_any(),
        );
        let state = active
            .getattr("generate_active_state")
            .unwrap()
            .call1((path(py, &case.root().join("repo")),))
            .unwrap();
        let mandates = state.getattr("standing_mandates").unwrap();
        assert!(mandates
            .eq(PyList::new(py, ["PROJECT_RULE: required"]).unwrap())
            .unwrap());
    });
}

#[test]
fn generation_uses_alternate_repository_headings_and_claim_path() {
    let case = Case::new();
    let alternate = case.mkdir("alternate");
    fs::write(
        alternate.join(".current_work.md"),
        "# Active Coordination\n## Alternate repository task\n",
    )
    .unwrap();
    let global = case.write("global.md", "## Wrong repository task\n");
    Python::attach(|py| {
        let seen = PyList::empty(py);
        let active = module(py, "conductor.active_state");
        let _global = AttrPatch::replace(active.as_any(), "CURRENT_WORK_PATH", &path(py, &global));
        let observed = seen.clone().unbind();
        let claims = strict_callback(py, &["repo"], &[], {
            move |bound| {
                observed
                    .bind(bound.py())
                    .call_method1("append", (bound.get_item("repo")?.unwrap(),))?;
                Ok(PyList::empty(bound.py()).into_any().unbind())
            }
        });
        let _claims = AttrPatch::replace(active.as_any(), "parse_active_claims", claims.as_any());
        let state = active
            .getattr("generate_active_state")
            .unwrap()
            .call1((path(py, &alternate),))
            .unwrap();
        let headings = state.getattr("active_headings").unwrap();
        assert!(headings
            .eq(PyList::new(py, ["Alternate repository task"]).unwrap())
            .unwrap());
        assert!(seen
            .eq(PyList::new(py, [path(py, &alternate)]).unwrap())
            .unwrap());
    });
}

#[test]
fn validation_rejects_a_claim_expired_at_the_supplied_instant() {
    Python::attach(|py| {
        let active = module(py, "conductor.active_state");
        let datetime = PyModule::import(py, "datetime").unwrap();
        let now = datetime
            .getattr("datetime")
            .unwrap()
            .getattr("now")
            .unwrap()
            .call1((datetime.getattr("UTC").unwrap(),))
            .unwrap();
        let expires = now
            .call_method1(
                "__sub__",
                (datetime
                    .getattr("timedelta")
                    .unwrap()
                    .call1((0, 1))
                    .unwrap(),),
            )
            .unwrap();
        let state_args = PyDict::new(py);
        state_args
            .set_item("last_updated", now.call_method0("isoformat").unwrap())
            .unwrap();
        state_args
            .set_item(
                "active_claims",
                PyList::new(
                    py,
                    [active_claim(
                        py,
                        "claim-expired",
                        &expires.call_method0("isoformat").unwrap(),
                    )],
                )
                .unwrap(),
            )
            .unwrap();
        let state = active
            .getattr("ActiveState")
            .unwrap()
            .call((), Some(&state_args))
            .unwrap();
        let kw = PyDict::new(py);
        kw.set_item("now", &now).unwrap();
        let error = active
            .getattr("validate_active_state")
            .unwrap()
            .call((state,), Some(&kw))
            .unwrap_err();
        assert_error(
            py,
            error,
            &active.getattr("ActiveStateError").unwrap(),
            "expired claim",
        );
    });
}

#[test]
fn failed_generation_preserves_the_last_good_state_file() {
    let case = Case::new();
    let target = case.write("active_state.json", "{\"last_good\": true}\n");
    Python::attach(|py| {
        let active = module(py, "conductor.active_state");
        let fail = strict_callback(py, &[], &[], |bound| {
            Err(active_error(bound.py(), "claim store unreadable"))
        });
        let _generate = AttrPatch::replace(active.as_any(), "generate_active_state", fail.as_any());
        let error = active
            .getattr("save_active_state")
            .unwrap()
            .call1((path(py, &target),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &active.getattr("ActiveStateError").unwrap(),
            "claim store unreadable",
        );
        let payload = PyModule::import(py, "json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((fs::read_to_string(target).unwrap(),))
            .unwrap();
        let expected = PyDict::new(py);
        expected.set_item("last_good", true).unwrap();
        assert!(payload.eq(expected).unwrap());
    });
}

fn active_error(py: Python<'_>, message: &str) -> PyErr {
    let active = module(py, "conductor.active_state");
    PyErr::from_value(
        active
            .getattr("ActiveStateError")
            .unwrap()
            .call1((message,))
            .unwrap(),
    )
}
