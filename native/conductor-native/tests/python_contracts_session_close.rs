#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for bounded claim release and session retirement.

#[path = "python_contracts/active_state_session_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{create_claim, strict_callback, SessionRepo};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyList, PyModule, PyTuple};
use support::{assert_error, module, path, AttrPatch};

fn claim_id(claim: &Bound<'_, PyAny>) -> String {
    claim.getattr("claim_id").unwrap().extract().unwrap()
}

fn bool_value(py: Python<'_>, value: bool) -> Bound<'_, PyAny> {
    PyBool::new(py, value).to_owned().into_any()
}

fn kwargs<'py>(py: Python<'py>, values: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    for (name, value) in values {
        kwargs.set_item(name, value).unwrap();
    }
    kwargs
}

fn output_capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let io = PyModule::import(py, "io").unwrap();
    let capture = io.getattr("StringIO").unwrap().call0().unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let restore = AttrPatch::replace(sys.as_any(), stream, &capture);
    (capture, restore)
}

fn repo_path<'py>(py: Python<'py>, repo: &SessionRepo) -> Bound<'py, PyAny> {
    path(py, repo.root())
}

struct UnchangedIndexTracking {
    lock_paths: Py<PyList>,
    built: Py<PyList>,
    saved: Py<PyList>,
    _build: AttrPatch,
    _lock: AttrPatch,
    _result: AttrPatch,
    _save: AttrPatch,
}

fn install_unchanged_index_tracking(
    py: Python<'_>,
    memory: &Bound<'_, PyModule>,
) -> UnchangedIndexTracking {
    let lock_paths = PyList::empty(py).unbind();
    let built = PyList::empty(py).unbind();
    let saved = PyList::empty(py).unbind();
    let lock = PyModule::import(py, "threading")
        .unwrap()
        .getattr("Lock")
        .unwrap()
        .call0()
        .unwrap();
    let returned_lock = lock.clone().unbind();
    let lock_fn = strict_callback(py, &["path"], &[], {
        let paths = lock_paths.clone_ref(py);
        move |bound| {
            paths
                .bind(bound.py())
                .append(bound.get_item("path")?.unwrap())?;
            Ok(returned_lock.clone_ref(bound.py()))
        }
    });
    let legacy_build = strict_callback(py, &[], &[], |bound| Ok(bound.py().None()));
    let result = PyModule::import(py, "types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs(py, &[("changed", bool_value(py, false))])))
        .unwrap()
        .unbind();
    let build_result = strict_callback(py, &[], &["index_path"], {
        let built = built.clone_ref(py);
        move |bound| {
            let index_path = bound.get_item("index_path")?.unwrap();
            built.bind(bound.py()).append(index_path)?;
            Ok(result.clone_ref(bound.py()))
        }
    });
    let save_result = strict_callback(py, &["result", "path"], &[], {
        let saved = saved.clone_ref(py);
        move |bound| {
            saved.bind(bound.py()).append(PyTuple::new(
                bound.py(),
                [
                    bound.get_item("result")?.unwrap(),
                    bound.get_item("path")?.unwrap(),
                ],
            )?)?;
            Ok(bound.py().None())
        }
    });
    UnchangedIndexTracking {
        lock_paths,
        built,
        saved,
        _build: AttrPatch::replace(memory.as_any(), "build_index", legacy_build.as_any()),
        _lock: AttrPatch::replace(memory.as_any(), "index_write_lock", lock_fn.as_any()),
        _result: AttrPatch::replace(memory.as_any(), "build_index_result", build_result.as_any()),
        _save: AttrPatch::replace(memory.as_any(), "save_index_result", save_result.as_any()),
    }
}

#[test]
fn claim_release_requires_explicit_scope_or_all_owner_authorization() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let session = module(py, "conductor.session_close");
        let claim = create_claim(
            py,
            &ownership,
            repo.root(),
            "agent-alpha",
            "conductor/foo.py",
            "alpha task",
        );
        let cid = claim_id(&claim);
        let options = kwargs(
            py,
            &[("owner", "agent-alpha".into_pyobject(py).unwrap().into_any())],
        );
        let error = session
            .getattr("release_owner_claims")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap_err();
        assert_error(
            py,
            error,
            &session.getattr("SessionCloseError").unwrap(),
            "specify --claim-id <id>... or explicit --all-owner-claims",
        );
        let options = kwargs(
            py,
            &[
                ("owner", "agent-alpha".into_pyobject(py).unwrap().into_any()),
                ("all_owner_claims", bool_value(py, true)),
            ],
        );
        let released = session
            .getattr("release_owner_claims")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap();
        assert!(released.eq(PyTuple::new(py, [cid]).unwrap()).unwrap());
    });
}

#[test]
fn specific_claim_release_leaves_other_owner_claims_intact() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let session = module(py, "conductor.session_close");
        let first = create_claim(
            py,
            &ownership,
            repo.root(),
            "agent-alpha",
            "conductor/foo.py",
            "alpha task 1",
        );
        let second = create_claim(
            py,
            &ownership,
            repo.root(),
            "agent-alpha",
            "conductor/baz.py",
            "alpha task 2",
        );
        let first_id = claim_id(&first);
        let second_id = claim_id(&second);
        let options = kwargs(
            py,
            &[
                ("owner", "agent-alpha".into_pyobject(py).unwrap().into_any()),
                (
                    "claim_ids",
                    PyTuple::new(py, [first_id.clone()]).unwrap().into_any(),
                ),
            ],
        );
        let released = session
            .getattr("release_owner_claims")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap();
        assert!(released.eq(PyTuple::new(py, [first_id]).unwrap()).unwrap());
        let claims = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((repo_path(py, &repo),))
            .unwrap();
        let active = claims.get_item(0).unwrap();
        assert_eq!(active.len().unwrap(), 1);
        assert_eq!(claim_id(&active.get_item(0).unwrap()), second_id);
    });
}

#[test]
fn close_session_appends_handoff_refreshes_state_and_releases_claims() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let session = module(py, "conductor.session_close");
        let claim = create_claim(
            py,
            &ownership,
            repo.root(),
            "agent-close",
            "conductor/file1.py",
            "close task",
        );
        let cid = claim_id(&claim);
        let options = kwargs(
            py,
            &[
                ("owner", "agent-close".into_pyobject(py).unwrap().into_any()),
                (
                    "title",
                    "Completed test task".into_pyobject(py).unwrap().into_any(),
                ),
                (
                    "body",
                    "All tests passed cleanly."
                        .into_pyobject(py)
                        .unwrap()
                        .into_any(),
                ),
                (
                    "claim_ids",
                    PyTuple::new(py, [cid.clone()]).unwrap().into_any(),
                ),
                ("sync_memory_index", bool_value(py, false)),
            ],
        );
        let result = session
            .getattr("close_session")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap();
        assert_eq!(
            result
                .getattr("owner")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "agent-close"
        );
        assert!(result
            .getattr("claims_released")
            .unwrap()
            .eq(PyTuple::new(py, [cid]).unwrap())
            .unwrap());
        let entry: String = result.getattr("handoff_entry").unwrap().extract().unwrap();
        assert!(entry.contains("Completed test task"));
        assert_eq!(
            ownership
                .getattr("load_claims")
                .unwrap()
                .call1((repo_path(py, &repo),))
                .unwrap()
                .get_item(0)
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        let active_path = repo.root().join("conductor/active_state.json");
        assert!(active_path.is_file());
        let payload = PyModule::import(py, "json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((std::fs::read_to_string(active_path).unwrap(),))
            .unwrap();
        assert_eq!(
            payload
                .get_item("schema_version")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        let summary: String = session
            .getattr("format_summary")
            .unwrap()
            .call1((result,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(summary.contains("session-close SUCCESS"));
        assert!(summary.contains("Completed test task"));
    });
}

#[test]
fn close_session_preserves_all_three_validation_rows() {
    let cases = [
        ("", Some("Title"), Some("Body"), "owner is required"),
        (
            "test-agent",
            Some("Title only"),
            None,
            "both --title and --body must be provided together",
        ),
        (
            "test-agent",
            None,
            Some("Body only"),
            "both --title and --body must be provided together",
        ),
    ];
    for (owner, title, body, message) in cases {
        let repo = SessionRepo::new();
        Python::attach(|py| {
            let session = module(py, "conductor.session_close");
            let mut values = vec![("owner", owner.into_pyobject(py).unwrap().into_any())];
            if let Some(title) = title {
                values.push(("title", title.into_pyobject(py).unwrap().into_any()));
            } else {
                values.push(("title", py.None().into_bound(py)));
            }
            if let Some(body) = body {
                values.push(("body", body.into_pyobject(py).unwrap().into_any()));
            } else {
                values.push(("body", py.None().into_bound(py)));
            }
            let options = kwargs(py, &values);
            let error = session
                .getattr("close_session")
                .unwrap()
                .call((repo_path(py, &repo),), Some(&options))
                .unwrap_err();
            assert_error(
                py,
                error,
                &session.getattr("SessionCloseError").unwrap(),
                message,
            );
        });
    }
}

#[test]
fn main_cli_emits_json_for_a_successful_close() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let session = module(py, "conductor.session_close");
        let claim = create_claim(
            py,
            &ownership,
            repo.root(),
            "cli-agent",
            "conductor/cli_file.py",
            "cli task",
        );
        let cid = claim_id(&claim);
        let (stdout, _restore) = output_capture(py, "stdout");
        let args = [
            "--repo",
            repo.root().to_str().unwrap(),
            "--owner",
            "cli-agent",
            "--title",
            "CLI Close",
            "--body",
            "Done via CLI.",
            "--claim-id",
            &cid,
            "--no-memory-index",
            "--json",
        ];
        let argv = PyList::new(py, args).unwrap();
        assert_eq!(
            session
                .getattr("main")
                .unwrap()
                .call1((argv,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let output: String = stdout.call_method0("getvalue").unwrap().extract().unwrap();
        let data = PyModule::import(py, "json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((output,))
            .unwrap();
        assert_eq!(
            data.get_item("owner").unwrap().extract::<String>().unwrap(),
            "cli-agent"
        );
        assert_eq!(data.get_item("claims_released").unwrap().len().unwrap(), 1);
    });
}

#[test]
fn main_cli_reports_validation_failure_on_stderr_with_exit_two() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let session = module(py, "conductor.session_close");
        let (stderr, _restore) = output_capture(py, "stderr");
        let args = PyList::new(
            py,
            [
                "--repo",
                repo.root().to_str().unwrap(),
                "--owner",
                "cli-agent",
                "--title",
                "Only Title",
            ],
        )
        .unwrap();
        assert_eq!(
            session
                .getattr("main")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
        let output: String = stderr.call_method0("getvalue").unwrap().extract().unwrap();
        assert!(output.contains("session-close FAILED"));
    });
}

#[test]
fn memory_index_sync_uses_the_fixture_lock_and_selected_repository() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let session = module(py, "conductor.session_close");
        let memory = module(py, "conductor.memory_index");
        let called = PyList::empty(py).unbind();
        let lock_paths = PyList::empty(py).unbind();
        let lock = PyModule::import(py, "threading")
            .unwrap()
            .getattr("Lock")
            .unwrap()
            .call0()
            .unwrap();
        let observed_lock = lock.clone().unbind();
        let build = strict_callback(py, &["repo"], &[], {
            let called = called.clone_ref(py);
            move |bound| {
                let locked: bool = observed_lock
                    .bind(bound.py())
                    .call_method0("locked")?
                    .extract()?;
                assert!(locked, "memory index build must run under the write lock");
                let repo = bound.get_item("repo")?.unwrap();
                called.bind(bound.py()).append(repo)?;
                Ok(bound.py().None())
            }
        });
        let returned_lock = lock.clone().unbind();
        let index_lock = strict_callback(py, &["path"], &[], {
            let lock_paths = lock_paths.clone_ref(py);
            move |bound| {
                lock_paths
                    .bind(bound.py())
                    .append(bound.get_item("path")?.unwrap())?;
                Ok(returned_lock.clone_ref(bound.py()))
            }
        });
        let _build = AttrPatch::replace(memory.as_any(), "build_index", build.as_any());
        let _lock = AttrPatch::replace(memory.as_any(), "index_write_lock", index_lock.as_any());
        let options = kwargs(
            py,
            &[
                ("owner", "mem-agent".into_pyobject(py).unwrap().into_any()),
                ("all_owner_claims", bool_value(py, true)),
                ("sync_memory_index", bool_value(py, true)),
            ],
        );
        let result = session
            .getattr("close_session")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap();
        assert_eq!(
            result
                .getattr("memory_status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "ok"
        );
        assert!(called
            .bind(py)
            .eq(PyList::new(py, [repo_path(py, &repo)]).unwrap())
            .unwrap());
        assert!(lock_paths
            .bind(py)
            .eq(PyList::new(
                py,
                [path(
                    py,
                    &repo.root().join("research/cache/memory_index.jsonl")
                )],
            )
            .unwrap())
            .unwrap());
    });
}

#[test]
fn unchanged_memory_index_result_is_not_saved() {
    let repo = SessionRepo::new();
    Python::attach(|py| {
        let session = module(py, "conductor.session_close");
        let memory = module(py, "conductor.memory_index");
        let tracking = install_unchanged_index_tracking(py, &memory);
        let options = kwargs(
            py,
            &[
                ("owner", "mem-agent".into_pyobject(py).unwrap().into_any()),
                ("all_owner_claims", bool_value(py, true)),
                ("sync_memory_index", bool_value(py, true)),
            ],
        );
        let closed = session
            .getattr("close_session")
            .unwrap()
            .call((repo_path(py, &repo),), Some(&options))
            .unwrap();
        assert_eq!(
            closed
                .getattr("memory_status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "ok"
        );
        let expected = PyList::new(
            py,
            [path(
                py,
                &repo.root().join("research/cache/memory_index.jsonl"),
            )],
        )
        .unwrap();
        assert!(tracking.lock_paths.bind(py).eq(&expected).unwrap());
        assert!(tracking.built.bind(py).eq(&expected).unwrap());
        assert!(tracking.saved.bind(py).eq(PyList::empty(py)).unwrap());
    });
}
