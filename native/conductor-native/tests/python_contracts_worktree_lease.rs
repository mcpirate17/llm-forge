#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_worktree_lease.py (11 expanded cases).

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/worktree_migration_support.rs"]
#[allow(dead_code)]
mod workspace;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::fs;
use support::{assert_error, module, path, text};
use workspace::WorkspaceCase;

fn opened<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "datetime")
        .getattr("datetime")
        .unwrap()
        .call_method1("fromisoformat", ("2026-09-07T12:00:00+00:00",))
        .unwrap()
}

fn call_open<'py>(
    py: Python<'py>,
    tree: &std::path::Path,
    owner: &str,
    purpose: &str,
    hours: Option<f64>,
    fixed_now: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    if fixed_now {
        kwargs.set_item("now", opened(py)).unwrap();
    }
    let api = module(py, "conductor.worktree_lease");
    let args = (path(py, tree), owner, purpose);
    match hours {
        Some(hours) => api
            .getattr("open_lease")?
            .call((args.0, args.1, args.2, hours), Some(&kwargs)),
        None => api.getattr("open_lease")?.call(args, Some(&kwargs)),
    }
}

fn lease_tree(case: &WorkspaceCase) -> std::path::PathBuf {
    let tree = case.mkdir("tree");
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("branch", "x").unwrap();
        kwargs.set_item("now", opened(py)).unwrap();
        module(py, "conductor.worktree_lease")
            .getattr("open_lease")
            .unwrap()
            .call(
                (path(py, &tree), "llm-d5", "land the research pile", 8.0),
                Some(&kwargs),
            )
            .unwrap();
    });
    tree
}

fn lease_file(py: Python<'_>, tree: &std::path::Path) -> std::path::PathBuf {
    let name = text(
        &module(py, "conductor.worktree_lease")
            .getattr("LEASE_FILENAME")
            .unwrap(),
    );
    tree.join(name)
}

#[test]
fn lease_records_trimmed_owner_purpose_and_computed_deadline() {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let record = call_open(
            py,
            case.root(),
            "llm-d5",
            "  land the research pile  ",
            Some(6.0),
            true,
        )
        .unwrap()
        .cast_into::<PyDict>()
        .unwrap();
        assert!(record
            .get_item("owner")
            .unwrap()
            .unwrap()
            .eq("llm-d5")
            .unwrap());
        assert!(record
            .get_item("purpose")
            .unwrap()
            .unwrap()
            .eq("land the research pile")
            .unwrap());
        let deadline = module(py, "datetime")
            .getattr("datetime")
            .unwrap()
            .call_method1(
                "fromisoformat",
                (record.get_item("expires_at").unwrap().unwrap(),),
            )
            .unwrap();
        let delta = module(py, "datetime").getattr("timedelta").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("hours", 6).unwrap();
        let expected = opened(py)
            .call_method1("__add__", (delta.call((), Some(&kwargs)).unwrap(),))
            .unwrap();
        assert!(deadline.eq(expected).unwrap());
        let disk = fs::read_to_string(lease_file(py, case.root())).unwrap();
        let parsed = module(py, "json")
            .getattr("loads")
            .unwrap()
            .call1((disk,))
            .unwrap();
        assert!(parsed.eq(record).unwrap());
    });
}

#[test]
fn unexplained_or_unowned_lease_is_refused_without_file() {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let error = module(py, "conductor.worktree_lease")
            .getattr("LeaseError")
            .unwrap();
        assert_error(
            py,
            call_open(py, case.root(), "llm-d5", "   ", None, false).unwrap_err(),
            &error,
            "purpose",
        );
        assert_error(
            py,
            call_open(py, case.root(), "  ", "land the research pile", None, false).unwrap_err(),
            &error,
            "owner",
        );
        assert!(!lease_file(py, case.root()).exists());
    });
}

fn lease_hours_refused(hours: f64) {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let error = module(py, "conductor.worktree_lease")
            .getattr("LeaseError")
            .unwrap();
        assert_error(
            py,
            call_open(
                py,
                case.root(),
                "llm-d5",
                "land the research pile",
                Some(hours),
                false,
            )
            .unwrap_err(),
            &error,
            "hours",
        );
    });
}

#[test]
fn lease_hours_refuses_zero() {
    lease_hours_refused(0.0);
}

#[test]
fn lease_hours_refuses_negative() {
    lease_hours_refused(-1.0);
}

#[test]
fn lease_hours_refuses_over_one_week() {
    lease_hours_refused(169.0);
}

#[test]
fn absent_lease_is_none_but_invalid_json_and_schema_raise() {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let api = module(py, "conductor.worktree_lease");
        let read = api.getattr("read_lease").unwrap();
        let error = api.getattr("LeaseError").unwrap();
        assert!(read.call1((path(py, case.root()),)).unwrap().is_none());
        let file = lease_file(py, case.root());
        fs::write(&file, "{not json").unwrap();
        assert_error(
            py,
            read.call1((path(py, case.root()),)).unwrap_err(),
            &error,
            "cannot read",
        );
        fs::write(&file, "{\"schema\":\"something.else\"}").unwrap();
        assert_error(
            py,
            read.call1((path(py, case.root()),)).unwrap_err(),
            &error,
            "not a worktree-lease.v1",
        );
    });
}

#[test]
fn lease_missing_deadline_is_broken_not_empty() {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let record = call_open(
            py,
            case.root(),
            "llm-d5",
            "land the research pile",
            None,
            true,
        )
        .unwrap()
        .cast_into::<PyDict>()
        .unwrap();
        record.del_item("expires_at").unwrap();
        let json = module(py, "json");
        let body: String = json
            .getattr("dumps")
            .unwrap()
            .call1((record,))
            .unwrap()
            .extract()
            .unwrap();
        fs::write(lease_file(py, case.root()), body).unwrap();
        let api = module(py, "conductor.worktree_lease");
        assert_error(
            py,
            api.getattr("read_lease")
                .unwrap()
                .call1((path(py, case.root()),))
                .unwrap_err(),
            &api.getattr("LeaseError").unwrap(),
            "no expires_at",
        );
    });
}

#[test]
fn state_distinguishes_live_expired_and_unleased_with_overdue_minutes() {
    let case = WorkspaceCase::new();
    let live = lease_tree(&case);
    let bare = case.mkdir("bare");
    Python::attach(|py| {
        let state = module(py, "conductor.worktree_lease")
            .getattr("lease_state")
            .unwrap();
        let dt = module(py, "datetime").getattr("timedelta").unwrap();
        let plus = |hours: i32, minutes: i32| {
            let kwargs = PyDict::new(py);
            kwargs.set_item("hours", hours).unwrap();
            kwargs.set_item("minutes", minutes).unwrap();
            opened(py)
                .call_method1("__add__", (dt.call((), Some(&kwargs)).unwrap(),))
                .unwrap()
        };
        let kwargs = PyDict::new(py);
        kwargs.set_item("now", plus(7, 0)).unwrap();
        let paths = PyList::new(py, [path(py, &live), path(py, &bare)]).unwrap();
        let within = state
            .call((paths,), Some(&kwargs))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert_eq!(
            text(&within.get_item(0).unwrap().get_item("status").unwrap()),
            "leased"
        );
        assert_eq!(
            text(&within.get_item(1).unwrap().get_item("status").unwrap()),
            "unleased"
        );
        assert_eq!(
            within
                .get_item(0)
                .unwrap()
                .get_item("overdue_minutes")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        kwargs.set_item("now", plus(9, 30)).unwrap();
        let past = state
            .call(
                (PyList::new(py, [path(py, &live)]).unwrap(),),
                Some(&kwargs),
            )
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert_eq!(
            text(&past.get_item(0).unwrap().get_item("status").unwrap()),
            "expired"
        );
        assert_eq!(
            past.get_item(0)
                .unwrap()
                .get_item("overdue_minutes")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            90
        );
    });
}

#[test]
fn state_skips_registration_for_missing_directory() {
    let case = WorkspaceCase::new();
    Python::attach(|py| {
        let rows = module(py, "conductor.worktree_lease")
            .getattr("lease_state")
            .unwrap()
            .call1((PyList::new(py, [path(py, &case.root().join("never-existed"))]).unwrap(),))
            .unwrap();
        assert_eq!(rows.len().unwrap(), 0);
    });
}

#[test]
fn main_checkout_is_not_a_disposable_worktree() {
    let case = WorkspaceCase::new();
    let checkout = case.mkdir("checkout/.git");
    let linked = case.mkdir("linked");
    fs::write(
        linked.join(".git"),
        "gitdir: /elsewhere/.git/worktrees/linked\n",
    )
    .unwrap();
    Python::attach(|py| {
        let linked_fn = module(py, "conductor.worktree_lease")
            .getattr("is_linked_worktree")
            .unwrap();
        assert!(linked_fn
            .call1((path(py, &linked),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!linked_fn
            .call1((path(py, checkout.parent().unwrap()),))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn cli_defaults_owner_to_tree_name_and_prints_record() {
    let mut case = WorkspaceCase::new();
    case.case.remove_env("GOVERNANCE_OWNER");
    let tree = case.mkdir("llm-d5-research-pile");
    Python::attach(|py| {
        let io = module(py, "io");
        let stream = io.getattr("StringIO").unwrap().call0().unwrap();
        let redirect = module(py, "contextlib")
            .getattr("redirect_stdout")
            .unwrap()
            .call1((&stream,))
            .unwrap();
        redirect.call_method0("__enter__").unwrap();
        let api = module(py, "conductor.worktree_lease");
        let argv =
            PyList::new(py, ["open", tree.to_str().unwrap(), "--purpose", "land it"]).unwrap();
        let status = api.getattr("main").unwrap().call1((argv,));
        redirect
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        assert_eq!(status.unwrap().extract::<i32>().unwrap(), 0);
        let output: String = stream.call_method0("getvalue").unwrap().extract().unwrap();
        let record = module(py, "json")
            .getattr("loads")
            .unwrap()
            .call1((output,))
            .unwrap();
        assert!(record
            .get_item("owner")
            .unwrap()
            .eq("llm-d5-research-pile")
            .unwrap());
    });
}
