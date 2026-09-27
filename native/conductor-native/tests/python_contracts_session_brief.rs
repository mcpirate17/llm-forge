#![cfg(feature = "python-compat-tests")]
//! Rust assertions over the live session-brief API and isolated local fixtures.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, buffer_text, capture, clear_buffer, py_json, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

const INBOX: &str = concat!(
    "[UNREAD] aaa from=codex at=t1\n",
    "word word word word word word word word word word word word word word word word word word word word ",
    "word word word word word word word word word word word word word word word word word word word word ",
    "word word word word word word word word word word word word word word word word word word word word \n",
    "second line\n\n[UNREAD] bbb from=helm at=t2\nshort\n\n[UNREAD] ccc from=x at=t3\n\n"
);

fn compact_payload() -> Value {
    json!({"schema_version":1,"authority":"bounded-a2a-inbox","agent":"fable-5",
        "unread_only":true,"total":1,"shown":1,"omitted":0,"raw_bytes_not_injected":12,
        "messages":[{"id":"aaa","from":"codex","at":"t1","thread":"thread-1",
        "status":"open","requires_response":true,"summary":"short","raw_bytes":12}]})
}

fn sidecar_payload(py: Python<'_>) -> Bound<'_, PyAny> {
    PyTuple::new(
        py,
        [
            py_json(py, json!(["row"])),
            "matrix".into_pyobject(py).unwrap().into_any(),
        ],
    )
    .unwrap()
    .into_any()
}

// Models the original `lambda *a, **k` subprocess fixtures; fixed signatures
// use `strict_constant` below.
fn constant<'py>(py: Python<'py>, value: Bound<'py, PyAny>) -> Bound<'py, PyCFunction> {
    let value = value.unbind();
    PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, _| {
        Ok::<_, PyErr>(value.clone_ref(args.py()))
    })
    .unwrap()
}

fn strict_constant<'py>(
    py: Python<'py>,
    value: Bound<'py, PyAny>,
    positional: &[&str],
    keyword_only: &[&str],
) -> Bound<'py, PyCFunction> {
    let signature = signature(py, positional, keyword_only);
    let value = value.unbind();
    PyCFunction::new_closure(py, None, None, move |args: &Bound<'_, PyTuple>, kw| {
        bind_signature(&signature, args, kw)?;
        Ok::<_, PyErr>(value.clone_ref(args.py()))
    })
    .unwrap()
}

fn clear_query_cache(sb: &Bound<'_, PyModule>) {
    sb.getattr("_query_vector")
        .unwrap()
        .call_method0("cache_clear")
        .unwrap();
}

fn namespace<'py>(py: Python<'py>, values: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (key, value) in values {
        kwargs.set_item(key, value).unwrap();
    }
    py.import("types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn run_result<'py>(py: Python<'py>, code: i32, stdout: &str) -> Bound<'py, PyAny> {
    namespace(
        py,
        &[
            ("returncode", code.into_pyobject(py).unwrap().into_any()),
            ("stdout", stdout.into_pyobject(py).unwrap().into_any()),
        ],
    )
}

fn assert_inbox_command(
    args: &Bound<'_, PyTuple>,
    kw: &Bound<'_, PyDict>,
    expected: &[String],
) -> PyResult<()> {
    let command = if args.len() == 1 {
        assert!(!kw.contains("command")?);
        args.get_item(0)?
    } else {
        assert_eq!(args.len(), 0);
        kw.get_item("command")?.expect("required command")
    };
    assert!(command.eq(PyList::new(command.py(), expected)?)?);
    Ok(())
}

fn run_patch<'py>(
    py: Python<'py>,
    sb: &Bound<'py, PyModule>,
    result: Bound<'py, PyAny>,
) -> AttrPatch {
    let process = sb.getattr("subprocess").unwrap();
    AttrPatch::replace(&process, "run", constant(py, result).as_any())
}

#[test]
fn snippet_strips_frontmatter_and_truncates() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let snippet = sb.getattr("snippet").unwrap();
        assert_eq!(
            snippet
                .call1(("---\nid: X\n---\n\n# T\n\na  b\n",))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "# T a b"
        );
        let kw = PyDict::new(py);
        kw.set_item("limit", 20).unwrap();
        let output: String = snippet
            .call(("w ".repeat(300),), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(output.chars().count(), 20);
        assert!(output.ends_with('…'));
    });
}

#[test]
fn compact_inbox_previews_headers_and_caps_messages() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let compact = sb.getattr("compact_inbox").unwrap();
        let output: String = compact.call1((INBOX,)).unwrap().extract().unwrap();
        let lines = output.lines().collect::<Vec<_>>();
        let chars: usize = sb.getattr("PREVIEW_CHARS").unwrap().extract().unwrap();
        assert_eq!(lines[0], "[UNREAD] aaa from=codex at=t1");
        assert!(lines[1].starts_with("  word word") && lines[1].ends_with('…'));
        assert_eq!(lines[1].chars().count(), 2 + chars);
        assert!(output.contains("[UNREAD] bbb from=helm at=t2\n  short"));
        assert!(output.ends_with("[UNREAD] ccc from=x at=t3"));
        let kw = PyDict::new(py);
        kw.set_item("max_msgs", 1).unwrap();
        let capped: String = compact
            .call((INBOX,), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert!(capped.ends_with("(+2 more unread)"));
        assert_eq!(
            compact.call1(("",)).unwrap().extract::<String>().unwrap(),
            ""
        );
    });
}

fn claimed_repo(py: Python<'_>, case: &Case, sb: &Bound<'_, PyModule>) -> AttrPatch {
    let repo = case.root().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    std::fs::write(repo.join("a.py"), "X = 1\n").unwrap();
    std::fs::write(repo.join("b.py"), "X = 1\n").unwrap();
    let ownership = module(py, "conductor.candidate_review.ownership");
    let kw = PyDict::new(py);
    kw.set_item("owner", "codex").unwrap();
    kw.set_item("paths", ["a.py"]).unwrap();
    kw.set_item("justification", "j").unwrap();
    kw.set_item("max_minutes", 60).unwrap();
    ownership
        .getattr("create_claim")
        .unwrap()
        .call((path(py, &repo),), Some(&kw))
        .unwrap();
    AttrPatch::replace(sb.as_any(), "ROOT", &path(py, &repo))
}

#[test]
fn claims_for_paths_boundary_reports_load_failure() {
    let case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let _repo = claimed_repo(py, &case, &sb);
        let shape = signature(py, &["_repo"], &[]);
        let fail = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            bind_signature(&shape, args, kw)?;
            Err(pyo3::exceptions::PyOSError::new_err(
                "claims store unreadable",
            ))
        })
        .unwrap();
        let _patch = AttrPatch::replace(sb.as_any(), "load_claims", fail.as_any());
        let kw = PyDict::new(py);
        kw.set_item("repo", path(py, &case.root().join("repo")))
            .unwrap();
        let result: String = sb
            .getattr("claims_for_paths")
            .unwrap()
            .call((vec!["a.py"],), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, "CLAIMS: unavailable (claims store unreadable)");
    });
}

#[test]
fn claims_for_paths_reports_overlap_or_absence() {
    let case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let _repo = claimed_repo(py, &case, &sb);
        let repo = path(py, &case.root().join("repo"));
        let datetime = py.import("datetime").unwrap();
        let now = datetime
            .getattr("datetime")
            .unwrap()
            .call_method1(
                "now",
                (datetime
                    .getattr("timezone")
                    .unwrap()
                    .getattr("utc")
                    .unwrap(),),
            )
            .unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo", &repo).unwrap();
        kw.set_item("now", &now).unwrap();
        let claims = sb.getattr("claims_for_paths").unwrap();
        let hit: String = claims
            .call((vec!["a.py"],), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert!(hit.starts_with("CLAIMS overlapping your paths:\nclaims: 1 active"));
        assert!(hit.contains(" codex ") && hit.contains("\n    a.py"));
        assert_eq!(
            claims
                .call((vec!["b.py"],), Some(&kw))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "CLAIMS: none overlap b.py — claim before editing"
        );
        assert_eq!(
            claims
                .call((Vec::<String>::new(),), Some(&kw))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "CLAIMS: no paths given"
        );
        let later = now
            .call_method1(
                "__add__",
                (datetime
                    .getattr("timedelta")
                    .unwrap()
                    .call(
                        (),
                        Some(&{
                            let d = PyDict::new(py);
                            d.set_item("hours", 2).unwrap();
                            d
                        }),
                    )
                    .unwrap(),),
            )
            .unwrap();
        kw.set_item("now", later).unwrap();
        let expired: String = claims
            .call((vec!["a.py"],), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert!(expired.contains("none overlap"));
    });
}

#[test]
fn inbox_preview_handles_missing_agent_and_failures() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let preview = sb.getattr("inbox_preview").unwrap();
        assert_eq!(
            preview
                .call1((py.None(),))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            ""
        );
        let expected = vec![
            py.import("sys")
                .unwrap()
                .getattr("executable")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "-m".into(),
            "conductor.agent_a2a".into(),
            "inbox".into(),
            "--as-name".into(),
            "fable-5".into(),
            "--unread".into(),
            "--compact".into(),
            "--max-messages".into(),
            "8".into(),
            "--preview-chars".into(),
            "140".into(),
            "--max-chars".into(),
            "1200".into(),
            "--json".into(),
        ];
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| {
                let kw = kw.expect("subprocess keywords");
                assert_inbox_command(args, kw, &expected)?;
                assert_eq!(kw.get_item("timeout")?.unwrap().extract::<u32>()?, 10);
                assert!(kw.get_item("capture_output")?.unwrap().extract::<bool>()?);
                assert!(kw.get_item("text")?.unwrap().extract::<bool>()?);
                assert!(!kw.get_item("check")?.unwrap().extract::<bool>()?);
                Ok::<_, PyErr>(run_result(args.py(), 0, &compact_payload().to_string()).unbind())
            },
        )
        .unwrap();
        let process = sb.getattr("subprocess").unwrap();
        let _patch = AttrPatch::replace(&process, "run", callback.as_any());
        let output: String = preview.call1(("fable-5",)).unwrap().extract().unwrap();
        let canonical = py.import("json").unwrap().getattr("dumps").unwrap();
        let kw = PyDict::new(py);
        kw.set_item("ensure_ascii", false).unwrap();
        kw.set_item("separators", (",", ":")).unwrap();
        kw.set_item("sort_keys", true).unwrap();
        let compact: String = canonical
            .call((py_json(py, compact_payload()),), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            output,
            format!("A2A unread (fable-5); full message by explicit show:\n{compact}")
        );
        let mut empty = compact_payload();
        empty["total"] = json!(0);
        empty["shown"] = json!(0);
        empty["messages"] = json!([]);
        empty["raw_bytes_not_injected"] = json!(0);
        let _empty = run_patch(py, &sb, run_result(py, 0, &empty.to_string()));
        assert_eq!(
            preview
                .call1(("fable-5",))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "A2A: no unread for fable-5"
        );
        let _exit = run_patch(py, &sb, run_result(py, 3, ""));
        assert_eq!(
            preview
                .call1(("fable-5",))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "A2A: inbox unavailable (exit 3)"
        );
    });
}

#[test]
fn inbox_preview_rejects_untrusted_compact_envelopes() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let max: usize = sb.getattr("MAX_INBOX_CHARS").unwrap().extract().unwrap();
        let mut bad = Vec::<Value>::new();
        bad.push(json!([]));
        for (key, value) in [
            ("schema_version", json!(2)),
            ("agent", json!("other")),
            ("omitted", json!(1)),
            ("total", json!(true)),
            ("extension", json!("x".repeat(max))),
        ] {
            let mut payload = compact_payload();
            payload[key] = value;
            bad.push(payload);
        }
        let mut raw = compact_payload();
        raw["messages"][0]["body"] = json!("full body must not cross the boundary");
        bad.insert(5, raw);
        let preview = sb.getattr("inbox_preview").unwrap();
        for payload in bad {
            let patch = run_patch(py, &sb, run_result(py, 0, &payload.to_string()));
            assert_eq!(
                preview
                    .call1(("fable-5",))
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "A2A: inbox unavailable (untrusted compact response)"
            );
            drop(patch);
        }
        let _invalid = run_patch(py, &sb, run_result(py, 0, "not-json"));
        assert_eq!(
            preview
                .call1(("fable-5",))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "A2A: inbox unavailable (invalid compact response)"
        );
    });
}

#[test]
fn inbox_preview_reports_subprocess_failure() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                assert_eq!(args.len(), 1);
                assert_eq!(
                    kw.unwrap().get_item("timeout")?.unwrap().extract::<u32>()?,
                    10
                );
                let process = args.py().import("subprocess")?;
                Err(pyo3::PyErr::from_value(
                    process
                        .getattr("TimeoutExpired")?
                        .call1(("agent_a2a", 10))?,
                ))
            },
        )
        .unwrap();
        let process = sb.getattr("subprocess").unwrap();
        let _patch = AttrPatch::replace(&process, "run", callback.as_any());
        let output: String = sb
            .getattr("inbox_preview")
            .unwrap()
            .call1(("fable-5",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.starts_with("A2A: inbox unavailable ("));
    });
}

#[test]
fn top_cards_formats_kb_retrieve_hits() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let kb = sb.getattr("kb_retrieve").unwrap();
        let card = namespace(
            py,
            &[
                ("name", "kb_a.md".into_pyobject(py).unwrap().into_any()),
                (
                    "text",
                    "---\nid: X\n---\n\n# Title\n\nBody line."
                        .into_pyobject(py)
                        .unwrap()
                        .into_any(),
                ),
            ],
        );
        let _index = AttrPatch::replace(
            &kb,
            "load_index",
            strict_constant(py, py_json(py, json!(["row"])), &[], &[]).as_any(),
        );
        let cards = pyo3::types::PyList::new(py, [card]).unwrap();
        let _query = AttrPatch::replace(
            &kb,
            "query_index",
            strict_constant(
                py,
                cards.into_any(),
                &["task", "rows", "top_k", "embedder"],
                &[],
            )
            .as_any(),
        );
        assert!(sb
            .getattr("top_cards")
            .unwrap()
            .call1(("do it",))
            .unwrap()
            .eq(PyList::new(py, ["- kb_a.md: # Title Body line."]).unwrap())
            .unwrap());
    });
}

#[test]
fn brief_degrades_cleanly_when_local_indexes_are_missing() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let kb = sb.getattr("kb_retrieve").unwrap();
        let memory = sb.getattr("memory_vectors").unwrap();
        let state = py_json(py, json!({"standing_mandates":["GRAPH_GATE: call"]}));
        let _state = AttrPatch::replace(
            sb.as_any(),
            "load_state",
            strict_constant(py, state, &["refresh"], &[]).as_any(),
        );
        let _claims = AttrPatch::replace(
            sb.as_any(),
            "claims_for_paths",
            strict_constant(
                py,
                "CLAIMS: none".into_pyobject(py).unwrap().into_any(),
                &["paths"],
                &[],
            )
            .as_any(),
        );
        let _inbox = AttrPatch::replace(
            sb.as_any(),
            "inbox_preview",
            strict_constant(
                py,
                "".into_pyobject(py).unwrap().into_any(),
                &["agent"],
                &[],
            )
            .as_any(),
        );
        let shape = signature(py, &[], &[]);
        let missing = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            bind_signature(&shape, args, kw)?;
            Err(pyo3::exceptions::PyFileNotFoundError::new_err(
                "clean checkout",
            ))
        })
        .unwrap();
        let _kb = AttrPatch::replace(&kb, "load_index", missing.as_any());
        let _memory = AttrPatch::replace(&memory, "load_sidecar", missing.as_any());
        let _tasks = AttrPatch::replace(
            sb.as_any(),
            "task_previews",
            strict_constant(py, py_json(py, json!([])), &["task"], &[]).as_any(),
        );
        let result: String = sb
            .getattr("brief")
            .unwrap()
            .call1(("repair clean checkout", vec!["conductor/example.py"]))
            .unwrap()
            .extract()
            .unwrap();
        assert!(result.contains("TASK: repair clean checkout"));
        assert!(result.contains("MANDATES: GRAPH_GATE"));
        assert!(result.contains("CLAIMS: none"));
    });
}

#[test]
fn kb_and_memory_retrieval_share_one_query_embedding() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let kb = sb.getattr("kb_retrieve").unwrap();
        let memory = sb.getattr("memory_vectors").unwrap();
        let vector = py_json(py, json!([0.25, 0.75])).unbind();
        let calls = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let embed_shape = signature(py, &["text"], &["purpose"]);
        let embed = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            let vector = vector.clone_ref(py);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&embed_shape, args, kw)?;
                calls.lock().unwrap().push((
                    bound.getattr("arguments")?.get_item("text")?.extract()?,
                    kw.unwrap().get_item("purpose")?.unwrap().extract()?,
                ));
                Ok(vector.clone_ref(args.py()))
            }
        })
        .unwrap();
        let _embed = AttrPatch::replace(&kb, "embed_text", embed.as_any());
        let _index = AttrPatch::replace(
            &kb,
            "load_index",
            strict_constant(py, py_json(py, json!(["kb-row"])), &[], &[]).as_any(),
        );
        let cards_k: usize = sb.getattr("CARDS_K").unwrap().extract().unwrap();
        let memory_k: usize = sb.getattr("MEMORY_K").unwrap().extract().unwrap();
        let query_shape = signature(py, &["_task", "_rows"], &["top_k", "embedder"]);
        let query = PyCFunction::new_closure(py, None, None, {
            let vector = vector.clone_ref(py);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Vec<String>> {
                bind_signature(&query_shape, args, kw)?;
                let kw = kw.unwrap();
                assert_eq!(kw.get_item("top_k")?.unwrap().extract::<usize>()?, cards_k);
                let embedded = kw.get_item("embedder")?.unwrap().call1(("ignored",))?;
                assert!(embedded.is(vector.bind(args.py())));
                Ok(Vec::new())
            }
        })
        .unwrap();
        let _query = AttrPatch::replace(&kb, "query_index", query.as_any());
        let _sidecar = AttrPatch::replace(
            &memory,
            "load_sidecar",
            strict_constant(py, sidecar_payload(py), &[], &[]).as_any(),
        );
        let search_shape = signature(py, &["query", "_rows", "_matrix"], &["top_k"]);
        let search = PyCFunction::new_closure(py, None, None, {
            let vector = vector.clone_ref(py);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Vec<String>> {
                let bound = bind_signature(&search_shape, args, kw)?;
                assert!(bound
                    .getattr("arguments")?
                    .get_item("query")?
                    .is(vector.bind(args.py())));
                assert_eq!(
                    kw.unwrap().get_item("top_k")?.unwrap().extract::<usize>()?,
                    memory_k
                );
                Ok(Vec::new())
            }
        })
        .unwrap();
        let _search = AttrPatch::replace(&memory, "search", search.as_any());
        clear_query_cache(&sb);
        assert!(sb
            .getattr("top_cards")
            .unwrap()
            .call1(("shared task",))
            .unwrap()
            .eq(PyList::empty(py))
            .unwrap());
        assert!(sb
            .getattr("memory_previews")
            .unwrap()
            .call1(("shared task",))
            .unwrap()
            .eq(PyList::empty(py))
            .unwrap());
        let instruct: String = kb.getattr("QUERY_INSTRUCT").unwrap().extract().unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            [(format!("{instruct}shared task"), "query".to_owned())]
        );
        clear_query_cache(&sb);
    });
}

#[test]
fn task_previews_opens_index_read_only() {
    let case = Case::new();
    let db = case.root().join("notes.sqlite");
    Python::attach(|py| {
        let sqlite = py.import("sqlite3").unwrap();
        sqlite
            .getattr("connect")
            .unwrap()
            .call1((path(py, &db),))
            .unwrap()
            .call_method0("close")
            .unwrap();
        let sb = module(py, "conductor.session_brief");
        let notes = module(py, "conductor.index_notes");
        let _db = AttrPatch::replace(
            sb.as_any(),
            "notes_db_path",
            strict_constant(py, path(py, &db).into_any(), &["_root"], &[]).as_any(),
        );
        let search_shape = signature(py, &["connection", "task"], &["limit", "source"]);
        let search = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&search_shape, args, kw)?;
                assert_eq!(
                    bound
                        .getattr("arguments")?
                        .get_item("task")?
                        .extract::<String>()?,
                    "pending work"
                );
                let kw = kw.unwrap();
                assert_eq!(kw.get_item("limit")?.unwrap().extract::<u32>()?, 3);
                assert_eq!(
                    kw.get_item("source")?.unwrap().extract::<String>()?,
                    "tasks"
                );
                let result = bound
                    .getattr("arguments")?
                    .get_item("connection")?
                    .call_method1("execute", ("CREATE TABLE forbidden_write (value INTEGER)",));
                assert!(result.is_err());
                let err = result.unwrap_err();
                assert!(err.is_instance(
                    args.py(),
                    &args.py().import("sqlite3")?.getattr("OperationalError")?
                ));
                assert!(err.to_string().contains("readonly"));
                Ok(py_json(
                    args.py(),
                    json!([{"title":"Task","snippet":"Pending","path":"task.md"}]),
                )
                .unbind())
            },
        )
        .unwrap();
        let _search = AttrPatch::replace(notes.as_any(), "search_notes", search.as_any());
        assert!(sb
            .getattr("task_previews")
            .unwrap()
            .call1(("pending work",))
            .unwrap()
            .eq(PyList::new(py, ["- Task: Pending (task.md)"]).unwrap())
            .unwrap());
    });
}

#[test]
fn build_brief_assembles_sections_and_bounds_size() {
    let _case = Case::new();
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let build = sb.getattr("build_brief").unwrap();
        let kw = PyDict::new(py);
        kw.set_item("task", "fix x").unwrap();
        kw.set_item("state", py_json(py, json!({"standing_mandates":["GRAPH_GATE: call the graph","CLAIM_REQUIRED: claim",7],"active_headings":["h1","h2","h3","h4","h5"]}))).unwrap();
        kw.set_item("claims_text", "CLAIMS: none").unwrap();
        kw.set_item("inbox_text", "A2A: no unread for me").unwrap();
        kw.set_item("cards", vec!["- kb_a.md: alpha", "- kb_b.md: beta"])
            .unwrap();
        let text: String = build.call((), Some(&kw)).unwrap().extract().unwrap();
        assert_eq!(
            text.lines().take(2).collect::<Vec<_>>(),
            ["TASK: fix x", "MANDATES: GRAPH_GATE, CLAIM_REQUIRED"]
        );
        assert!(text.contains("- h4") && !text.contains("- h5"));
        assert!(text.find("CLAIMS: none").unwrap() < text.find("A2A:").unwrap());
        assert!(text.find("A2A:").unwrap() < text.find("CARDS:").unwrap());
        kw.set_item("task", "t").unwrap();
        kw.set_item("state", py_json(py, json!({}))).unwrap();
        kw.set_item("claims_text", "x".repeat(5000)).unwrap();
        kw.set_item("inbox_text", "").unwrap();
        kw.set_item("cards", Vec::<String>::new()).unwrap();
        let huge: String = build.call((), Some(&kw)).unwrap().extract().unwrap();
        let max: usize = sb.getattr("MAX_BRIEF_CHARS").unwrap().extract().unwrap();
        assert_eq!(huge.chars().count(), max);
        assert!(huge.ends_with('…'));
        assert!(!huge.contains("HEADINGS"));
    });
}

#[test]
fn brief_orchestrates_and_main_prints() {
    let mut case = Case::new();
    case.set_env("A2A_AGENT_NAME", "env-agent");
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let _state = AttrPatch::replace(
            sb.as_any(),
            "load_state",
            strict_constant(
                py,
                py_json(py, json!({"standing_mandates":["M: x"]})),
                &["refresh"],
                &[],
            )
            .as_any(),
        );
        let claims_shape = signature(py, &["paths"], &[]);
        let claims = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kw| -> PyResult<String> {
                let bound = bind_signature(&claims_shape, args, kw)?;
                Ok(format!(
                    "CLAIMS for {}",
                    bound
                        .getattr("arguments")?
                        .get_item("paths")?
                        .repr()?
                        .extract::<String>()?
                ))
            },
        )
        .unwrap();
        let _claims = AttrPatch::replace(sb.as_any(), "claims_for_paths", claims.as_any());
        let inbox_shape = signature(py, &["agent"], &[]);
        let inbox = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kw| -> PyResult<String> {
                let bound = bind_signature(&inbox_shape, args, kw)?;
                Ok(format!(
                    "A2A for {}",
                    bound
                        .getattr("arguments")?
                        .get_item("agent")?
                        .extract::<String>()?
                ))
            },
        )
        .unwrap();
        let _inbox = AttrPatch::replace(sb.as_any(), "inbox_preview", inbox.as_any());
        let cards_shape = signature(py, &["task"], &[]);
        let cards = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kw| -> PyResult<Vec<String>> {
                let bound = bind_signature(&cards_shape, args, kw)?;
                Ok(vec![format!(
                    "- card for {}",
                    bound
                        .getattr("arguments")?
                        .get_item("task")?
                        .extract::<String>()?
                )])
            },
        )
        .unwrap();
        let _cards = AttrPatch::replace(sb.as_any(), "top_cards", cards.as_any());
        let empty = strict_constant(py, py_json(py, json!([])), &["task"], &[]);
        let _memory = AttrPatch::replace(sb.as_any(), "memory_previews", empty.as_any());
        let _tasks = AttrPatch::replace(sb.as_any(), "task_previews", empty.as_any());
        let output: String = sb
            .getattr("brief")
            .unwrap()
            .call1(("do it", vec!["p.py"]))
            .unwrap()
            .extract()
            .unwrap();
        assert!(output.contains("CLAIMS for ['p.py']") && output.contains("A2A for env-agent"));
        assert!(output.contains("- card for do it"));
        assert_error(
            py,
            sb.getattr("brief").unwrap().call1((" ",)).unwrap_err(),
            &py.get_type::<pyo3::exceptions::PyValueError>().into_any(),
            "empty",
        );
        assert_brief_cli(py, &sb);
    });
}

fn assert_brief_cli(py: Python<'_>, sb: &Bound<'_, PyModule>) {
    let (stdout, _stdout) = capture(py, "stdout");
    assert_eq!(
        sb.getattr("main")
            .unwrap()
            .call1((vec![
                "brief", "--task", "do it", "--paths", "p.py", "--agent", "me"
            ],))
            .unwrap()
            .extract::<i32>()
            .unwrap(),
        0
    );
    assert!(buffer_text(&stdout).contains("A2A for me"));
    clear_buffer(&stdout);
    let input = py
        .import("io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call1((INBOX,))
        .unwrap();
    let sys = py.import("sys").unwrap();
    let _stdin = AttrPatch::replace(sys.as_any(), "stdin", &input);
    assert_eq!(
        sb.getattr("main")
            .unwrap()
            .call1((vec!["a2a-compact"],))
            .unwrap()
            .extract::<i32>()
            .unwrap(),
        0
    );
    assert!(buffer_text(&stdout).starts_with("[UNREAD] aaa"));
}

#[test]
fn compact_inbox_passes_already_compact_input_through() {
    let _case = Case::new();
    let compact = "A2A compact agent=fable-5 total=2 shown=2 omitted=0\n[open] aaa from=codex thread=t1 response=yes\n  first summary\n[working] bbb from=helm thread=t2\n  second summary\nraw bytes withheld from context: 999\n";
    Python::attach(|py| {
        let sb = module(py, "conductor.session_brief");
        let is_compact = sb.getattr("is_compact_inbox").unwrap();
        assert!(is_compact
            .call1((compact,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!is_compact
            .call1((INBOX,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let compact_inbox = sb.getattr("compact_inbox").unwrap();
        assert_eq!(
            compact_inbox
                .call1((compact,))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            compact.trim()
        );
        let kw = PyDict::new(py);
        kw.set_item("max_msgs", 1).unwrap();
        assert_eq!(
            compact_inbox
                .call((compact,), Some(&kw))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            compact.trim()
        );
    });
}
