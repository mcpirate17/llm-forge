#![cfg(feature = "python-compat-tests")]
//! Bounded A2A graph context contracts from test_a2a_graph_context.py.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use support::{module, path, AttrPatch, Case};

fn json_object<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = py
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn write_module(root: &std::path::Path, relative: &str, source: &str) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, source).unwrap();
}

fn summary<'py>(
    graph: &Bound<'py, PyModule>,
    skeleton: &str,
    callers: &[String],
    callees: &[String],
) -> Bound<'py, PyAny> {
    let relationship = graph.getattr("GraphRelationship").unwrap();
    let rels = |names: &[String]| -> Vec<Py<PyAny>> {
        names
            .iter()
            .map(|name| {
                relationship
                    .call1((name, "CALLS", "pkg/caller.py"))
                    .unwrap()
                    .unbind()
            })
            .collect()
    };
    graph
        .getattr("FileContextSummary")
        .unwrap()
        .call1((
            "pkg/mod.py",
            skeleton,
            vec!["ping"],
            rels(callers),
            rels(callees),
            "ok",
        ))
        .unwrap()
}

fn return_mock<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    py.import("unittest.mock")
        .unwrap()
        .getattr("MagicMock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

#[test]
fn refs_are_repo_bounded_deduplicated_and_limited() {
    let case = Case::new();
    let repo = case.root().join("repo");
    write_module(&repo, "pkg/a.py", "def first() -> None: ...\n");
    write_module(&repo, "pkg/b.py", "def second() -> None: ...\n");
    write_module(&repo, "pkg/c.py", "def third() -> None: ...\n");
    write_module(case.root(), "outside.py", "def escape() -> None: ...\n");
    Python::attach(|py| {
        let graph = module(py, "conductor.a2a_graph_context");
        let fragments = json_object(
            py,
            json!([
                {"message_id":"m-1","body":format!(
                    "reject {}::escape then inspect pkg/a.py::first and pkg/b.py::second",
                    case.root().join("outside.py").display()),"data_json":""},
                {"message_id":"m-2","body":format!(
                    "repeat {}::first then pkg/c.py::third",repo.join("pkg/a.py").display()),
                    "data_json":""}
            ]),
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_refs", 2).unwrap();
        let refs = graph
            .getattr("extract_context_refs")
            .unwrap()
            .call((path(py, &repo), fragments), Some(&kwargs))
            .unwrap();
        let refs: Vec<Py<PyAny>> = refs.extract().unwrap();
        assert_eq!(refs.len(), 2);
        let keys: Vec<(String, String)> = refs
            .iter()
            .map(|r| {
                let r = r.bind(py);
                (
                    r.getattr("path").unwrap().extract().unwrap(),
                    r.getattr("symbol").unwrap().extract().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            [
                ("pkg/a.py".into(), "first".into()),
                ("pkg/b.py".into(), "second".into())
            ]
        );
        let ids: Vec<String> = refs[0]
            .bind(py)
            .getattr("message_ids")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(ids, ["m-1", "m-2"]);
    });
}

#[test]
fn bounded_context_requests_graph_and_includes_ast_relationships() {
    let case = Case::new();
    let repo = case.root().join("repo");
    write_module(
        &repo,
        "pkg/mod.py",
        "def ping() -> str:\n    return 'pong'\n",
    );
    Python::attach(|py| {
        let context = module(py, "conductor.a2a_graph_context");
        let graph = module(py, "conductor.graph_context");
        let summary = summary(
            &graph,
            "def ping() -> str: ...",
            &["pkg.caller".into()],
            &["pkg.callee".into()],
        );
        let lookup = return_mock(py, &summary);
        let _patch = AttrPatch::replace(context.as_any(), "get_file_context", &lookup);
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_chars", 600).unwrap();
        let result = context
            .getattr("build_bounded_code_context")
            .unwrap()
            .call(
                (
                    path(py, &repo),
                    json_object(
                        py,
                        json!([
                            {"message_id":"m-graph","body":"pkg/mod.py::ping","data_json":""}
                        ]),
                    ),
                ),
                Some(&kwargs),
            )
            .unwrap();
        assert_eq!(
            lookup
                .getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        let call = lookup.getattr("call_args").unwrap();
        assert_eq!(
            call.get_item(0)
                .unwrap()
                .get_item(0)
                .unwrap()
                .str()
                .unwrap()
                .to_str()
                .unwrap(),
            repo.to_str().unwrap()
        );
        assert_eq!(
            call.get_item(0)
                .unwrap()
                .get_item(1)
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "pkg/mod.py"
        );
        let call_kwargs = call.get_item(1).unwrap();
        assert_eq!(
            call_kwargs
                .get_item("target_symbol")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "ping"
        );
        assert!(call_kwargs
            .get_item("with_graph")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(
            to_json(py, &result)["contexts"],
            json!([{
                "messages":["m-graph"],"path":"pkg/mod.py","symbol":"ping",
                "ast":"def ping() -> str: ...","callers":["pkg.caller"],
                "callees":["pkg.callee"],"graph_status":"ok"
            }])
        );
    });
}

#[test]
fn serialized_budget_excludes_raw_message_and_bounds_large_graph() {
    let case = Case::new();
    let repo = case.root().join("repo");
    write_module(&repo, "pkg/mod.py", "def ping() -> None: ...\n");
    let marker = "raw-message-marker-must-not-enter-context";
    Python::attach(|py| {
        let context = module(py, "conductor.a2a_graph_context");
        let graph = module(py, "conductor.graph_context");
        let relationships: Vec<String> = (0..8)
            .map(|i| format!("pkg.{}.{}", "caller".repeat(40), i))
            .collect();
        let summary = summary(
            &graph,
            &format!("def ping(value: str) -> str: {}", "detail ".repeat(300)),
            &relationships,
            &relationships,
        );
        let lookup = return_mock(py, &summary);
        let _patch = AttrPatch::replace(context.as_any(), "get_file_context", &lookup);
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_chars", 320).unwrap();
        let result = context
            .getattr("build_bounded_code_context")
            .unwrap()
            .call((path(py, &repo), json_object(py, json!([
                {"message_id":"m-budget","body":format!("{marker} inspect pkg/mod.py::ping"),
                 "data_json":""}
            ]))), Some(&kwargs))
            .unwrap();
        let json = py.import("json").unwrap();
        let encode_kwargs = PyDict::new(py);
        encode_kwargs.set_item("sort_keys", true).unwrap();
        encode_kwargs.set_item("separators", (",", ":")).unwrap();
        let serialized: String = json
            .getattr("dumps")
            .unwrap()
            .call((result,), Some(&encode_kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert!(serialized.len() <= 320);
        assert!(!serialized.contains(marker));
    });
}

#[test]
fn store_fragments_scan_only_bounded_prefixes() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let context = module(py, "conductor.a2a_graph_context");
        let store = a2a
            .getattr("A2aStore")
            .unwrap()
            .call1((path(py, case.root()), "tester-a"))
            .unwrap();
        store
            .call_method1(
                "record_inbound",
                (
                    "m-scan",
                    "tester-b",
                    "tester-a",
                    format!("{} pkg/late.py::missed", "b".repeat(512)),
                    format!("{} pkg/also_late.py::missed", "d".repeat(512)),
                ),
            )
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("scan_chars", 256).unwrap();
        let fragments = context
            .getattr("read_context_fragments")
            .unwrap()
            .call(
                (store.getattr("path").unwrap(), vec!["m-scan"]),
                Some(&kwargs),
            )
            .unwrap();
        assert_eq!(
            to_json(py, &fragments),
            json!([{
                "message_id":"m-scan", "body":"b".repeat(256), "data_json":"d".repeat(256)
            }])
        );
    });
}

#[test]
fn watch_once_attaches_context_and_marks_presentation() {
    let case = Case::new();
    let repo = case.root().join("repo");
    write_module(
        &repo,
        "pkg/mod.py",
        "def ping() -> str:\n    return 'pong'\n",
    );
    let state = case.root().join("state");
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let cli = module(py, "conductor.a2a_cli");
        let context = module(py, "conductor.a2a_graph_context");
        let store = a2a
            .getattr("A2aStore")
            .unwrap()
            .call1((path(py, &state), "tester-a"))
            .unwrap();
        store
            .call_method1(
                "record_inbound",
                (
                    "m-watch",
                    "tester-b",
                    "tester-a",
                    "Please inspect pkg/mod.py::ping",
                    py.None(),
                ),
            )
            .unwrap();
        let args = vec![
            "--state-dir".to_owned(),
            state.to_string_lossy().into_owned(),
            "watch".into(),
            "--as-name".into(),
            "tester-a".into(),
            "--once".into(),
            "--json".into(),
            "--repo-root".into(),
            repo.to_string_lossy().into_owned(),
        ];
        let io = py.import("io").unwrap();
        let output = io.getattr("StringIO").unwrap().call0().unwrap();
        let sys = py.import("sys").unwrap();
        let _patch = AttrPatch::replace(&sys, "stdout", &output);
        assert_eq!(
            cli.getattr("main")
                .unwrap()
                .call1((&args,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let text: String = output.call_method0("getvalue").unwrap().extract().unwrap();
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            payload["code_context"]["authority"],
            context
                .getattr("AUTHORITY")
                .unwrap()
                .extract::<String>()
                .unwrap()
        );
        assert_eq!(payload["code_context"]["contexts"][0]["path"], "pkg/mod.py");
        assert!(payload["code_context"]["contexts"][0]["ast"]
            .as_str()
            .unwrap()
            .contains("def ping() -> str:"));
        output.call_method1("seek", (0,)).unwrap();
        output.call_method1("truncate", (0,)).unwrap();
        assert_eq!(
            cli.getattr("main")
                .unwrap()
                .call1((&args,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            output
                .call_method0("getvalue")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            ""
        );
    });
}

#[test]
fn no_reference_avoids_graph_lookup() {
    let case = Case::new();
    Python::attach(|py| {
        let context = module(py, "conductor.a2a_graph_context");
        let mock = py
            .import("unittest.mock")
            .unwrap()
            .getattr("MagicMock")
            .unwrap()
            .call0()
            .unwrap();
        let _patch = AttrPatch::replace(context.as_any(), "get_file_context", &mock);
        let result = context
            .getattr("build_bounded_code_context")
            .unwrap()
            .call1((
                path(py, case.root()),
                json_object(
                    py,
                    json!([
                        {"message_id":"m-none","body":"status is green","data_json":"{}"}
                    ]),
                ),
            ))
            .unwrap();
        let envelope = to_json(py, &result);
        assert_eq!(envelope["contexts"], json!([]));
        assert_eq!(envelope["omitted_refs"], 0);
        assert_eq!(
            mock.getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
    });
}
