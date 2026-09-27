#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for bounded SessionStart text, policy, and state loading.

#[path = "python_contracts/session_preamble_support.rs"]
mod preamble_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use preamble_support::{capture, fixture_repo, policy, render, state, ModulePatch};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn compact<'py>(
    py: Python<'py>,
    preamble: &Bound<'py, PyModule>,
    state: &Bound<'py, PyAny>,
    repo: &Path,
) -> String {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, repo)).unwrap();
    preamble
        .getattr("compact_state")
        .unwrap()
        .call((state,), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

fn write_state(py: Python<'_>, file: &Path) {
    let json = module(py, "json");
    let payload: String = json
        .getattr("dumps")
        .unwrap()
        .call1((state(py),))
        .unwrap()
        .extract()
        .unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, payload).unwrap();
}

fn json_value<'py>(py: Python<'py>, raw: &str) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((raw,))
        .unwrap()
}

fn cli(py: Python<'_>, preamble: &Bound<'_, PyModule>, args: Vec<String>) -> (i64, String, String) {
    capture(py, || {
        preamble
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn bound_path(
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    expected: &Path,
) -> PyResult<String> {
    if args.len() != 1 || kwargs.is_some_and(|kwargs| !kwargs.is_empty()) {
        return Err(PyTypeError::new_err("expected exactly one path argument"));
    }
    let actual = args.get_item(0)?;
    let pathlib_path = module(args.py(), "pathlib").getattr("Path")?;
    if !actual.is_instance(&pathlib_path)? || !actual.eq(path(args.py(), expected))? {
        return Err(PyTypeError::new_err("expected the selected pathlib.Path"));
    }
    Ok(text(&actual))
}

fn state_saver<'py>(
    py: Python<'py>,
    expected: &Path,
) -> (Bound<'py, PyCFunction>, Arc<Mutex<Vec<String>>>) {
    let fresh = json_value(py, r#"{"schema_version":1,"last_updated":"fresh"}"#).unbind();
    let to_dict =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            if !args.is_empty() || kwargs.is_some_and(|kwargs| !kwargs.is_empty()) {
                return Err(PyTypeError::new_err("to_dict expects no arguments"));
            }
            Ok(fresh.clone_ref(args.py()))
        })
        .unwrap();
    let state_kw = PyDict::new(py);
    state_kw.set_item("to_dict", to_dict).unwrap();
    let result_state = module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&state_kw))
        .unwrap()
        .unbind();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let expected = expected.to_path_buf();
    let save = PyCFunction::new_closure(py, None, None, {
        let calls = Arc::clone(&calls);
        move |args, kwargs| -> PyResult<Py<PyAny>> {
            calls
                .lock()
                .unwrap()
                .push(bound_path(args, kwargs, &expected)?);
            Ok(result_state.clone_ref(args.py()))
        }
    })
    .unwrap();
    (save, calls)
}

#[test]
fn compact_state_omits_claim_paths() {
    let case = Case::new();
    let repo = fixture_repo(&case);
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let output = compact(py, &preamble, state(py).as_any(), &repo);
        for expected in [
            "NOVEL_MECHANISMS_ONLY",
            "MEMORY_RETRIEVE",
            "heading-a",
            "CLAIMS: 1 active",
            "7317",
            "zero approval authority",
            "never gate work or runs on local output",
            ".current_work.md",
            "MUTATION",
            "mutate ONLY the files you changed",
            "automatic engines only",
            "hand-authored mutants",
            "never repo-wide",
            "DELEGATE: searches touching >3 files",
            "ast_context_tool/query_graph",
        ] {
            assert!(output.contains(expected), "missing {expected:?}");
        }
        assert!(!output.contains("session_preamble.py"));
        assert!(!output.contains("should not appear"));
    });
}

#[test]
fn inject_stays_under_budget() {
    let case = Case::new();
    let repo = fixture_repo(&case);
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let output: String = render(
            py,
            &preamble,
            state(py).as_any(),
            Some(&repo),
            "grok",
            &"peer ".repeat(2_000),
            None,
        )
        .extract()
        .unwrap();
        let max: usize = preamble
            .getattr("MAX_INJECT_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.chars().count() <= max);
        assert!(output.contains("python -m conductor.kb_retrieve"));
    });
}

#[test]
fn a2a_summary_requires_explicit_show_for_full_message() {
    let _case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let output: String = render(
            py,
            &preamble,
            state(py).as_any(),
            None,
            "codex-efficiency",
            "[open] message-1 from=peer\n  bounded summary",
            None,
        )
        .extract()
        .unwrap();
        for expected in [
            "A2A compact (codex-efficiency); retrieve only when needed",
            "`python -m conductor.agent_a2a show --as-name codex-efficiency <id>`",
            "`python -m conductor.agent_a2a read --as-name codex-efficiency <id>`",
            "bounded summary",
        ] {
            assert!(output.contains(expected), "missing {expected:?}");
        }
    });
}

#[test]
fn hook_payload_shape() {
    let case = Case::new();
    let repo = fixture_repo(&case);
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let kwargs = PyDict::new(py);
        kwargs.set_item("state", state(py)).unwrap();
        kwargs.set_item("repo", path(py, &repo)).unwrap();
        let payload = preamble
            .getattr("hook_payload")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let output = payload.get_item("hookSpecificOutput").unwrap();
        assert_eq!(
            text(&output.get_item("hookEventName").unwrap()),
            "SessionStart"
        );
        assert!(text(&output.get_item("additionalContext").unwrap()).contains("MISSION"));
        module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call1((payload,))
            .unwrap();
    });
}

#[test]
fn a2a_context_requires_both_fields_and_budget_truncation_is_exact() {
    let _case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let current = state(py);
        let unnamed: String = render(
            py,
            &preamble,
            current.as_any(),
            None,
            "   ",
            "private summary",
            None,
        )
        .extract()
        .unwrap();
        let unsummarized: String =
            render(py, &preamble, current.as_any(), None, "peer", "   ", None)
                .extract()
                .unwrap();
        assert!(!unnamed.contains("A2A compact"));
        assert!(!unnamed.contains("private summary"));
        assert!(!unsummarized.contains("A2A compact"));
        let clipped: String = render(
            py,
            &preamble,
            current.as_any(),
            None,
            "peer",
            &"summary ".repeat(40),
            Some(120),
        )
        .extract()
        .unwrap();
        assert_eq!(clipped.chars().count(), 120);
        assert!(clipped.ends_with('…'));
        let kwargs = PyDict::new(py);
        kwargs.set_item("event_name", "Resume").unwrap();
        kwargs.set_item("state", current).unwrap();
        let payload = preamble
            .getattr("hook_payload")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        assert_eq!(
            text(
                &payload
                    .get_item("hookSpecificOutput")
                    .unwrap()
                    .get_item("hookEventName")
                    .unwrap()
            ),
            "Resume"
        );
    });
}

#[test]
fn root_policy_is_rendered_before_live_state_summary() {
    let case = Case::new();
    let configured = fixture_repo(&case);
    let neutral = case.mkdir("no-policy");
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let session_policy = module(py, "conductor.session_policy");
        for repo in [&configured, &neutral] {
            let policy = session_policy
                .getattr("load_session_policy")
                .unwrap()
                .call1((path(py, repo),))
                .unwrap();
            let mandates: Vec<String> = policy
                .getattr("standing_mandates")
                .unwrap()
                .extract()
                .unwrap();
            let lines: Vec<String> = policy.getattr("preamble").unwrap().extract().unwrap();
            let state = PyDict::new(py);
            state.set_item("standing_mandates", &mandates).unwrap();
            state
                .set_item("active_claims", Vec::<String>::new())
                .unwrap();
            let output = compact(py, &preamble, state.as_any(), repo);
            let ids = mandates
                .iter()
                .map(|item| item.split(':').next().unwrap())
                .collect::<Vec<_>>();
            let mut expected = lines;
            expected.push(format!(
                "MANDATES: {}",
                if ids.is_empty() {
                    "none".to_owned()
                } else {
                    ids.join(", ")
                }
            ));
            expected.push("CLAIMS: 0 active. Inspect with `make governance-claims`.".to_owned());
            assert_eq!(output, expected.join("\n"));
        }
    });
}

#[test]
fn canonical_foreign_state_uses_the_foreign_policy() {
    let case = Case::new();
    let repo = case.mkdir("foreign");
    policy(&repo, &["FOREIGN: injected"], &["FOREIGN_RULE: required"]);
    let state_file = repo.join("conductor/active_state.json");
    fs::create_dir_all(state_file.parent().unwrap()).unwrap();
    fs::write(
        &state_file,
        r#"{"standing_mandates":["FOREIGN_RULE: required"],"active_claims":[]}"#,
    )
    .unwrap();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let (code, stdout, _) = cli(
            py,
            &preamble,
            vec![
                "text".into(),
                "--state".into(),
                state_file.display().to_string(),
            ],
        );
        assert_eq!(code, 0);
        assert!(stdout.contains("FOREIGN: injected"));
        assert!(!stdout.contains("MISSION: Beat frontier models"));
    });
}

#[test]
fn noncanonical_state_requires_repo_and_canonical_state_cannot_conflict() {
    let case = Case::new();
    let snapshot = case.root().join("snapshot.json");
    let first = case.root().join("first");
    let second = case.root().join("second");
    policy(&first, &["FIRST"], &[]);
    policy(&second, &["SECOND"], &[]);
    Python::attach(|py| {
        write_state(py, &snapshot);
        let preamble = module(py, "conductor.session_preamble");
        let (error, _, stderr) = capture(py, || {
            preamble
                .getattr("main")
                .unwrap()
                .call1((vec!["text", "--state", snapshot.to_str().unwrap()],))
                .unwrap_err()
        });
        assert!(error
            .matches(py, &module(py, "builtins").getattr("SystemExit").unwrap())
            .unwrap());
        assert_eq!(
            error
                .value(py)
                .getattr("code")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            2
        );
        assert!(stderr.contains("--repo is required when --state is not"));
        let canonical = first.join("conductor/active_state.json");
        write_state(py, &canonical);
        let (error, _, stderr) = capture(py, || {
            preamble
                .getattr("main")
                .unwrap()
                .call1((vec![
                    "text",
                    "--state",
                    canonical.to_str().unwrap(),
                    "--repo",
                    second.to_str().unwrap(),
                ],))
                .unwrap_err()
        });
        assert!(error
            .matches(py, &module(py, "builtins").getattr("SystemExit").unwrap())
            .unwrap());
        assert_eq!(
            error
                .value(py)
                .getattr("code")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            2
        );
        assert!(
            stderr.contains("--repo conflicts with the repository implied by canonical --state")
        );
        let (code, _, _) = cli(
            py,
            &preamble,
            vec![
                "text".into(),
                "--state".into(),
                snapshot.display().to_string(),
                "--repo".into(),
                first.display().to_string(),
            ],
        );
        assert_eq!(code, 0);
    });
}

#[test]
fn text_cli() {
    let case = Case::new();
    let file = case.root().join("active_state.json");
    Python::attach(|py| {
        write_state(py, &file);
        let preamble = module(py, "conductor.session_preamble");
        let (code, _, _) = cli(
            py,
            &preamble,
            vec![
                "text".into(),
                "--state".into(),
                file.display().to_string(),
                "--repo".into(),
                case.root().display().to_string(),
            ],
        );
        assert_eq!(code, 0);
    });
}

#[test]
fn load_state_refreshes_only_canonical_cache_and_rejects_malformed_explicit_cache() {
    let case = Case::new();
    let refreshed = case.write(
        "canonical-active-state.json",
        r#"{"schema_version":1,"last_updated":"cached"}"#,
    );
    let explicit = case.write(
        "active_state.json",
        r#"{"schema_version":1,"last_updated":"cached"}"#,
    );
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let _canonical = AttrPatch::replace(
            preamble.as_any(),
            "ACTIVE_STATE_PATH",
            &path(py, &refreshed),
        );
        let fake = PyModule::new(py, "conductor.active_state").unwrap();
        let (save, calls) = state_saver(py, &refreshed);
        fake.add("save_active_state", &save).unwrap();
        let _stub = ModulePatch::new(py, "conductor.active_state", &fake);
        let load = preamble.getattr("load_state").unwrap();
        let fresh = load.call1((path(py, &refreshed),)).unwrap();
        assert!(fresh
            .eq(json_value(
                py,
                r#"{"schema_version":1,"last_updated":"fresh"}"#
            ))
            .unwrap());
        assert_eq!(*calls.lock().unwrap(), [refreshed.display().to_string()]);
        let cached = load.call1((path(py, &explicit),)).unwrap();
        assert!(cached
            .eq(json_value(
                py,
                r#"{"schema_version":1,"last_updated":"cached"}"#
            ))
            .unwrap());
        assert_eq!(*calls.lock().unwrap(), [refreshed.display().to_string()]);
        fs::write(&explicit, "not json").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("refresh", false).unwrap();
        assert_error(
            py,
            load.call((path(py, &explicit),), Some(&kwargs))
                .unwrap_err(),
            &preamble.getattr("PreambleError").unwrap(),
            "unreadable",
        );
    });
}

#[test]
fn load_state_rejects_missing_and_non_object() {
    let case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let load = preamble.getattr("load_state").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("refresh", false).unwrap();
        let missing = case.root().join("absent.json");
        assert_error(
            py,
            load.call((path(py, &missing),), Some(&kwargs)).unwrap_err(),
            &preamble.getattr("PreambleError").unwrap(),
            "does not exist",
        );
        let nonobject = case.write("active_state.json", "[]\n");
        assert_error(
            py,
            load.call((path(py, &nonobject),), Some(&kwargs))
                .unwrap_err(),
            &preamble.getattr("PreambleError").unwrap(),
            "must be an object",
        );
    });
}

#[test]
fn compact_state_skips_non_string_mandates() {
    let _case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let payload = json_value(
            py,
            r#"{"standing_mandates":["KEEP: yes",12,""],"active_headings":[12,"keep-me"],"active_claims":[]}"#,
        );
        let output: String = preamble
            .getattr("compact_state")
            .unwrap()
            .call1((payload,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.contains("KEEP"));
        assert!(output.contains("keep-me"));
    });
}

#[test]
fn compact_state_retains_later_valid_values_and_structural_labels() {
    let _case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let payload = json_value(
            py,
            r#"{"standing_mandates":[12,"LATER: required"],"active_headings":["visible heading"],"active_claims":{"not":"a list"}}"#,
        );
        let output: String = preamble
            .getattr("compact_state")
            .unwrap()
            .call1((payload,))
            .unwrap()
            .extract()
            .unwrap();
        for expected in [
            "MANDATES: LATER",
            "CLAIMS: 0 active",
            "HEADINGS:\n- visible heading",
        ] {
            assert!(output.contains(expected));
        }
        let empty = json_value(py, r#"{"standing_mandates":[],"active_claims":[]}"#);
        let output: String = preamble
            .getattr("compact_state")
            .unwrap()
            .call1((empty,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.contains("MANDATES: none"));
    });
}

#[test]
fn hook_cli_and_load_error() {
    let case = Case::new();
    let file = case.root().join("active_state.json");
    Python::attach(|py| {
        write_state(py, &file);
        let preamble = module(py, "conductor.session_preamble");
        let (code, stdout, _) = cli(
            py,
            &preamble,
            vec![
                "hook".into(),
                "--state".into(),
                file.display().to_string(),
                "--repo".into(),
                case.root().display().to_string(),
            ],
        );
        assert_eq!(code, 0);
        assert!(stdout.contains("SessionStart"));
        let missing = case.root().join("nope.json");
        let (code, _, stderr) = cli(
            py,
            &preamble,
            vec![
                "text".into(),
                "--state".into(),
                missing.display().to_string(),
                "--repo".into(),
                case.root().display().to_string(),
            ],
        );
        assert_eq!(code, 2);
        assert!(stderr.contains("ERROR"));
    });
}

#[test]
fn load_state_refresh_failure() {
    let _case = Case::new();
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let fake = PyModule::new(py, "conductor.active_state").unwrap();
        let canonical = preamble.getattr("ACTIVE_STATE_PATH").unwrap();
        let expected = std::path::PathBuf::from(text(&canonical));
        let boom = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
            bound_path(args, kwargs, &expected)?;
            Err(pyo3::exceptions::PyOSError::new_err("cannot refresh"))
        })
        .unwrap();
        fake.add("save_active_state", boom).unwrap();
        let _stub = ModulePatch::new(py, "conductor.active_state", &fake);
        assert_error(
            py,
            preamble
                .getattr("load_state")
                .unwrap()
                .call1((canonical,))
                .unwrap_err(),
            &preamble.getattr("PreambleError").unwrap(),
            "refresh failed",
        );
    });
}

#[test]
fn render_without_state_reads_the_selected_repository_without_refresh() {
    let case = Case::new();
    let repo = case.mkdir("foreign");
    policy(&repo, &["FOREIGN: selected"], &["FOREIGN_RULE: required"]);
    Python::attach(|py| {
        let preamble = module(py, "conductor.session_preamble");
        let payload = json_value(
            py,
            r#"{"standing_mandates":["FOREIGN_RULE: required"],"active_claims":[]}"#,
        )
        .unbind();
        let expected = repo.join("conductor/active_state.json");
        let calls = Arc::new(Mutex::new(Vec::<(String, Option<bool>)>::new()));
        let loader = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            move |args, kwargs| -> PyResult<Py<PyAny>> {
                if args.len() != 1 || kwargs.is_some_and(|kwargs| kwargs.len() > 1) {
                    return Err(PyTypeError::new_err(
                        "expected path and optional refresh keyword",
                    ));
                }
                let refresh_value = if let Some(kwargs) = kwargs {
                    if !kwargs.is_empty() && !kwargs.contains("refresh")? {
                        return Err(PyTypeError::new_err("unexpected keyword argument"));
                    }
                    kwargs.get_item("refresh")?
                } else {
                    None
                };
                let refresh = match refresh_value {
                    Some(value) if !value.is_none() => Some(value.extract::<bool>()?),
                    _ => None,
                };
                let actual = args.get_item(0)?;
                let pathlib_path = module(args.py(), "pathlib").getattr("Path")?;
                if !actual.is_instance(&pathlib_path)? || !actual.eq(path(args.py(), &expected))? {
                    return Err(PyTypeError::new_err("expected the selected pathlib.Path"));
                }
                calls.lock().unwrap().push((text(&actual), refresh));
                Ok(payload.clone_ref(args.py()))
            }
        })
        .unwrap();
        let _patch = AttrPatch::replace(preamble.as_any(), "load_state", &loader);
        let render_kwargs = PyDict::new(py);
        render_kwargs.set_item("repo", path(py, &repo)).unwrap();
        let output: String = preamble
            .getattr("render_text")
            .unwrap()
            .call((), Some(&render_kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.contains("FOREIGN: selected"));
        assert_eq!(
            *calls.lock().unwrap(),
            [(
                repo.join("conductor/active_state.json")
                    .display()
                    .to_string(),
                None
            )]
        );
    });
}

#[test]
fn cli_reports_invalid_selected_policy_without_a_traceback() {
    let case = Case::new();
    let repo = case.mkdir("foreign");
    let state_file = repo.join("conductor/active_state.json");
    Python::attach(|py| {
        write_state(py, &state_file);
        fs::write(repo.join("pyproject.toml"), "[tool").unwrap();
        let preamble = module(py, "conductor.session_preamble");
        let (code, _, stderr) = cli(
            py,
            &preamble,
            vec![
                "text".into(),
                "--state".into(),
                state_file.display().to_string(),
            ],
        );
        assert_eq!(code, 2);
        assert!(stderr.contains("ERROR:"));
        assert!(!stderr.contains("Traceback"));
    });
}

#[test]
fn text_cli_newline_depends_on_a2a_summary() {
    let case = Case::new();
    let file = case.root().join("active_state.json");
    Python::attach(|py| {
        write_state(py, &file);
        let preamble = module(py, "conductor.session_preamble");
        let basic = vec![
            "text".into(),
            "--state".into(),
            file.display().to_string(),
            "--repo".into(),
            case.root().display().to_string(),
        ];
        let (code, stdout, _) = cli(py, &preamble, basic.clone());
        assert_eq!(code, 0);
        assert!(stdout.ends_with('\n'));
        let mut with_summary = basic;
        with_summary.extend([
            "--a2a-name".into(),
            "peer".into(),
            "--a2a-summary".into(),
            "summary".into(),
        ]);
        let (code, stdout, _) = cli(py, &preamble, with_summary);
        assert_eq!(code, 0);
        assert!(stdout.ends_with("summary"));
        assert!(!stdout.ends_with("summary\n"));
    });
}
