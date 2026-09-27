#![cfg(feature = "python-compat-tests")]
//! In-process A2A transport, durable state, and bounded CLI contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, clear_buffer, json_value, py_json};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use serde_json::{json, Value};
use std::path::Path;
use support::{assert_error, module, path, Case};

const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn gate(gate: Value) -> Value {
    json!({"kind":"gate-review-request","gate":gate,"fingerprint":FINGERPRINT,
        "artifact_paths":["research/reports/x/gate_3.json"]})
}

fn coordination(thread: &str, summary: &str, status: &str, supersedes: Vec<&str>) -> Value {
    json!({"kind":"coordination-v2","thread_id":thread,"summary":summary,
        "status":status,"requires_response":true,"supersedes":supersedes})
}

fn message(id: &str, data: Option<Value>) -> Value {
    let mut parts = vec![json!({"text":"gate 3 please"})];
    if let Some(data) = data {
        parts.push(json!({"data":data}));
    }
    json!({"messageId":id,"role":"ROLE_USER","parts":parts,"metadata":{"sender":"tester-b"}})
}

fn rpc(message: Value) -> Value {
    json!({"jsonrpc":"2.0","id":"t1","method":"SendMessage","params":{"message":message}})
}

fn store<'py>(py: Python<'py>, a2a: &Bound<'py, PyModule>, root: &Path) -> Bound<'py, PyAny> {
    a2a.getattr("A2aStore")
        .unwrap()
        .call1((path(py, root), "tester-a"))
        .unwrap()
}

fn record<'py>(store: &Bound<'py, PyAny>, id: &str, body: &str, data: Option<Value>) {
    store
        .call_method1(
            "record_inbound",
            (
                id,
                "tester-b",
                "tester-a",
                body,
                data.map(|value| value.to_string()),
            ),
        )
        .unwrap();
}

fn rows(store: &Bound<'_, PyAny>, unread: bool) -> Value {
    let rows = store.call_method1("rows", (unread, 10)).unwrap();
    rows.cast::<PyList>()
        .expect("rows must retain its Python list shape");
    json_value(&rows)
}

fn record_agent<'py>(a2a: &Bound<'py, PyModule>) -> Bound<'py, PyAny> {
    a2a.getattr("AgentRecord")
        .unwrap()
        .call1(("tester-a", 7399, "t".repeat(24)))
        .unwrap()
}

fn client<'py>(
    py: Python<'py>,
    a2a: &Bound<'py, PyModule>,
    record: &Bound<'py, PyAny>,
    store: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    let app = a2a
        .getattr("build_app")
        .unwrap()
        .call1((record, store))
        .unwrap();
    let test_client = py
        .import("starlette.testclient")
        .unwrap()
        .getattr("TestClient")
        .unwrap()
        .call1((app,))
        .unwrap();
    test_client.call_method0("__enter__").unwrap();
    test_client
}

fn close_client(client: &Bound<'_, PyAny>) {
    client
        .call_method1(
            "__exit__",
            (client.py().None(), client.py().None(), client.py().None()),
        )
        .unwrap();
}

fn headers<'py>(
    py: Python<'py>,
    a2a: &Bound<'py, PyModule>,
    record: &Bound<'py, PyAny>,
    token: bool,
    version: bool,
) -> Bound<'py, PyDict> {
    let headers = PyDict::new(py);
    if token {
        headers
            .set_item(
                a2a.getattr("TOKEN_HEADER").unwrap(),
                record.getattr("token").unwrap(),
            )
            .unwrap();
    }
    if version {
        headers
            .set_item(
                a2a.getattr("VERSION_HEADER").unwrap(),
                a2a.getattr("PROTOCOL_VERSION_1_0").unwrap(),
            )
            .unwrap();
    }
    headers
}

fn post<'py>(
    py: Python<'py>,
    client: &Bound<'py, PyAny>,
    request: Value,
    headers: &Bound<'py, PyDict>,
) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    kw.set_item("json", py_json(py, request)).unwrap();
    kw.set_item("headers", headers).unwrap();
    client.call_method("post", ("/",), Some(&kw)).unwrap()
}

fn main_args(root: &Path, args: &[&str]) -> Vec<String> {
    let mut result = vec!["--state-dir".to_owned(), root.display().to_string()];
    result.extend(args.iter().map(|s| (*s).to_owned()));
    result
}

#[test]
fn rpc_rejects_missing_or_wrong_token_for_both_rows() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        for token in [None, Some("wrong-token")] {
            if token.is_none() {
                let agent = record_agent(&a2a);
                let journal = store(py, &a2a, &case.root().join("card"));
                let client = client(py, &a2a, &agent, &journal);
                let path: String = a2a
                    .getattr("AGENT_CARD_WELL_KNOWN_PATH")
                    .unwrap()
                    .extract()
                    .unwrap();
                let response = client.call_method1("get", (path,)).unwrap();
                assert_eq!(
                    response
                        .getattr("status_code")
                        .unwrap()
                        .extract::<u16>()
                        .unwrap(),
                    200
                );
                let card = json_value(&response.call_method0("json").unwrap());
                assert_eq!(card["name"], "tester-a");
                assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
                assert_eq!(card["supportedInterfaces"][0]["protocolVersion"], "1.0");
                let skills = card["skills"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["id"].as_str().unwrap())
                    .collect::<Vec<_>>();
                assert!(
                    skills.contains(&"coordination") && skills.contains(&"gate-review-request")
                );
                close_client(&client);
            }
            let agent = record_agent(&a2a);
            let journal = store(
                py,
                &a2a,
                &case
                    .root()
                    .join(if token.is_none() { "missing" } else { "wrong" }),
            );
            let client = client(py, &a2a, &agent, &journal);
            let head = headers(py, &a2a, &agent, false, true);
            if let Some(token) = token {
                head.set_item(a2a.getattr("TOKEN_HEADER").unwrap(), token)
                    .unwrap();
            }
            let response = post(py, &client, rpc(message("m-1", None)), &head);
            assert_eq!(
                response
                    .getattr("status_code")
                    .unwrap()
                    .extract::<u16>()
                    .unwrap(),
                401
            );
            assert!(json_value(&response.call_method0("json").unwrap())
                .get("error")
                .is_some());
            close_client(&client);
        }
    });
}

fn inbound_receipt(py: Python<'_>, a2a: &Bound<'_, PyModule>, root: &Path) {
    let agent = record_agent(a2a);
    let journal = store(py, a2a, root);
    let client = client(py, a2a, &agent, &journal);
    let head = headers(py, a2a, &agent, true, true);
    let response = post(
        py,
        &client,
        rpc(message("m-1", Some(gate(json!(3))))),
        &head,
    );
    assert_eq!(
        response
            .getattr("status_code")
            .unwrap()
            .extract::<u16>()
            .unwrap(),
        200
    );
    let data = json_value(&response.call_method0("json").unwrap());
    let receipts = data["result"]["message"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.get("data"))
        .collect::<Vec<_>>();
    assert!(!receipts.is_empty());
    assert_eq!(
        receipts[0],
        &json!({"kind":"delivery-receipt","message_id":"m-1","recipient":"tester-a"})
    );
    let entries = rows(&journal, true);
    let entries = entries.as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|v| v["message_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["m-1"]
    );
    let row = &entries[0];
    assert_eq!(row["direction"], "inbound");
    assert_eq!(row["sender"], "tester-b");
    assert_eq!(row["recipient"], "tester-a");
    assert_eq!(row["body"], "gate 3 please");
    let data = py
        .import("json")
        .unwrap()
        .call_method1("loads", (row["data_json"].as_str().unwrap(),))
        .unwrap();
    assert!(data.eq(py_json(py, gate(json!(3)))).unwrap());
    assert!(row["read_at"].is_null());
    close_client(&client);
}

fn inbound_rejection(py: Python<'_>, a2a: &Bound<'_, PyModule>, root: &Path, kind: &str) {
    let agent = record_agent(a2a);
    let journal = store(py, a2a, root);
    let client = client(py, a2a, &agent, &journal);
    let head = headers(py, a2a, &agent, true, kind != "version");
    let mut request = rpc(message(
        "m-1",
        if kind == "invalid" {
            Some(gate(json!("three")))
        } else {
            None
        },
    ));
    if kind == "legacy" {
        request["method"] = json!("message/send");
    }
    let response = post(py, &client, request, &head);
    let payload = json_value(&response.call_method0("json").unwrap());
    assert!(payload.get("error").is_some());
    if kind == "legacy" {
        assert_eq!(payload["error"]["code"], -32601);
    }
    assert_eq!(rows(&journal, kind == "version"), json!([]));
    close_client(&client);
}

#[test]
fn inbound_is_deduplicated_by_message_id() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        inbound_receipt(py, &a2a, &case.root().join("receipt"));
        for kind in ["version", "legacy", "invalid"] {
            inbound_rejection(py, &a2a, &case.root().join(kind), kind);
        }
        let agent = record_agent(&a2a);
        let journal = store(py, &a2a, &case.root().join("dedup"));
        let client = client(py, &a2a, &agent, &journal);
        let head = headers(py, &a2a, &agent, true, true);
        for _ in 0..2 {
            let response = post(py, &client, rpc(message("m-1", None)), &head);
            assert_eq!(
                response
                    .getattr("status_code")
                    .unwrap()
                    .extract::<u16>()
                    .unwrap(),
                200
            );
        }
        let entries = rows(&journal, false);
        let ids = entries
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["message_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["m-1"]);
        close_client(&client);
    });
}

fn sql_rows(store: &Bound<'_, PyAny>, query: &str) -> Vec<Value> {
    let py = store.py();
    let context = store.call_method0("connect").unwrap();
    let connection = context.call_method0("__enter__").unwrap();
    let cursor = connection.call_method1("execute", (query,)).unwrap();
    let rows = cursor.call_method0("fetchall").unwrap();
    let dict = py.import("builtins").unwrap().getattr("dict").unwrap();
    let result = rows
        .cast::<PyList>()
        .unwrap()
        .iter()
        .map(|row| json_value(&dict.call1((row,)).unwrap()))
        .collect();
    context
        .call_method1("__exit__", (py.None(), py.None(), py.None()))
        .unwrap();
    result
}

fn run_main(a2a: &Bound<'_, PyModule>, args: Vec<String>) {
    let code: i32 = a2a
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(code, 0);
}

#[test]
fn default_inbox_is_bounded_json_and_raw_content_requires_full_or_show() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let journal = store(py, &a2a, case.root());
        let private_body = "raw-private-body-".repeat(100);
        let data = coordination("thread-1", "Safe bounded summary", "open", vec![]);
        record(&journal, "compact-1", &private_body, Some(data.clone()));
        let (stdout, _patch) = capture(py, "stdout");
        run_main(
            &a2a,
            main_args(
                case.root(),
                &[
                    "inbox",
                    "--as-name",
                    "tester-a",
                    "--json",
                    "--max-chars",
                    "512",
                ],
            ),
        );
        let compact_text = buffer_text(&stdout).trim().to_owned();
        let compact: Value = serde_json::from_str(&compact_text).unwrap();
        assert!(compact_text.len() <= 512);
        assert_eq!(compact["authority"], "bounded-a2a-inbox");
        assert_eq!(compact["messages"][0]["summary"], "Safe bounded summary");
        assert!(!compact_text.contains(&private_body));
        assert!(compact["messages"][0].get("data_json").is_none());
        clear_buffer(&stdout);
        run_main(
            &a2a,
            main_args(
                case.root(),
                &["inbox", "--as-name", "tester-a", "--full", "--json"],
            ),
        );
        let full: Value = serde_json::from_str(&buffer_text(&stdout)).unwrap();
        assert_eq!(full[0]["body"], private_body);
        assert_eq!(
            serde_json::from_str::<Value>(full[0]["data_json"].as_str().unwrap()).unwrap(),
            data
        );
        clear_buffer(&stdout);
        run_main(
            &a2a,
            main_args(
                case.root(),
                &["show", "--as-name", "tester-a", "--json", "compact-1"],
            ),
        );
        let shown: Value = serde_json::from_str(&buffer_text(&stdout)).unwrap();
        assert_eq!(shown["body"], private_body);
        assert_eq!(
            serde_json::from_str::<Value>(shown["data_json"].as_str().unwrap()).unwrap(),
            data
        );
    });
}

#[test]
fn coordination_v2_state_supersedes_same_thread_message() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let journal = store(py, &a2a, case.root());
        record(
            &journal,
            "old-message",
            "old raw body",
            Some(coordination("thread-1", "Old status", "open", vec![])),
        );
        record(
            &journal,
            "new-message",
            "new raw body",
            Some(coordination(
                "thread-1",
                "Replacement status",
                "in_progress",
                vec!["old-message"],
            )),
        );
        let states = sql_rows(&journal, "SELECT * FROM message_state ORDER BY message_id");
        let old = states
            .iter()
            .find(|r| r["message_id"] == "old-message")
            .unwrap();
        let new = states
            .iter()
            .find(|r| r["message_id"] == "new-message")
            .unwrap();
        assert_eq!(new["thread_id"], "thread-1");
        assert_eq!(new["summary"], "Replacement status");
        assert_eq!(new["protocol_status"], "in_progress");
        assert_eq!(new["requires_response"], 1);
        assert_eq!(old["protocol_status"], "superseded");
        assert!(!old["superseded_at"].is_null());
    });
}

#[test]
fn coordination_v2_rejects_cross_thread_supersession_atomically() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let journal = store(py, &a2a, case.root());
        record(
            &journal,
            "other-thread-message",
            "other",
            Some(coordination(
                "thread-other",
                "Bounded status",
                "open",
                vec![],
            )),
        );
        let error = journal
            .call_method1(
                "record_inbound",
                (
                    "invalid-successor",
                    "tester-b",
                    "tester-a",
                    "invalid",
                    coordination(
                        "thread-1",
                        "Bounded status",
                        "open",
                        vec!["other-thread-message"],
                    )
                    .to_string(),
                ),
            )
            .unwrap_err();
        assert_error(
            py,
            error,
            &a2a.getattr("A2aError").unwrap(),
            "another sender/thread",
        );
        assert!(sql_rows(
            &journal,
            "SELECT 1 FROM messages WHERE message_id='invalid-successor'"
        )
        .is_empty());
        let original = sql_rows(&journal, "SELECT protocol_status, superseded_at FROM message_state WHERE message_id='other-thread-message'");
        assert_eq!(original.len(), 1);
        assert_eq!(original[0]["protocol_status"], "open");
        assert!(original[0]["superseded_at"].is_null());
    });
}

fn assert_mark_read(py: Python<'_>, a2a: &Bound<'_, PyModule>, root: &Path) {
    let journal = store(py, a2a, root);
    record(&journal, "m-2", "handoff", None);
    assert_eq!(rows(&journal, true).as_array().unwrap().len(), 1);
    journal.call_method1("mark_read", ("m-2",)).unwrap();
    assert_eq!(rows(&journal, true), json!([]));
    assert_eq!(rows(&journal, false).as_array().unwrap().len(), 1);
}

fn assert_payload_validation(py: Python<'_>, a2a: &Bound<'_, PyModule>) {
    let validate = a2a.getattr("validate_data_payload").unwrap();
    let class = a2a.getattr("A2aError").unwrap();
    for number in [
        json!(1),
        json!(2),
        json!(3),
        json!(4),
        json!(5),
        json!(7),
        json!(7.0),
    ] {
        let result: String = validate
            .call1((py_json(py, gate(number)),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, "gate-review-request");
    }
    for number in [
        json!(0),
        json!(6),
        json!(6.0),
        json!(8),
        json!(-1),
        json!(6.5),
    ] {
        assert_error(
            py,
            validate.call1((py_json(py, gate(number)),)).unwrap_err(),
            &class,
            "requires integer gate",
        );
    }
    let kind: String = validate
        .call1((py_json(py, json!({"kind":"coordination"})),))
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(kind, "coordination");
    let invalid = vec![
        json!("not-a-dict"),
        json!({"kind":"unknown-kind"}),
        json!({"kind":"gate-review-request"}),
        json!({"kind":"gate-review-request","gate":true,"fingerprint":FINGERPRINT,"artifact_paths":["a"]}),
        json!({"kind":"gate-review-request","gate":3,"fingerprint":"a".repeat(63),"artifact_paths":["a"]}),
        json!({"kind":"gate-review-request","gate":3,"fingerprint":FINGERPRINT,"artifact_paths":[]}),
        json!({"kind":"gate-review-request","gate":3,"fingerprint":FINGERPRINT,"artifact_paths":[3]}),
    ];
    for payload in invalid {
        assert_error(
            py,
            validate.call1((py_json(py, payload),)).unwrap_err(),
            &class,
            "",
        );
    }
}

#[test]
fn resolve_requires_read_and_then_records_lifecycle_state() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        assert_mark_read(py, &a2a, &case.root().join("mark-read"));
        assert_payload_validation(py, &a2a);
        let journal = store(py, &a2a, &case.root().join("resolve"));
        record(
            &journal,
            "resolve-me",
            "please resolve",
            Some(coordination("thread-1", "Bounded status", "open", vec![])),
        );
        assert_error(
            py,
            journal
                .call_method1("resolve", ("resolve-me",))
                .unwrap_err(),
            &a2a.getattr("A2aError").unwrap(),
            "cannot resolve unread",
        );
        journal.call_method1("mark_read", ("resolve-me",)).unwrap();
        journal.call_method1("resolve", ("resolve-me",)).unwrap();
        let state = sql_rows(
            &journal,
            "SELECT protocol_status, resolved_at FROM message_state WHERE message_id='resolve-me'",
        );
        assert_eq!(state.len(), 1);
        assert_eq!(state[0]["protocol_status"], "resolved");
        assert!(!state[0]["resolved_at"].is_null());
    });
}

#[test]
fn hold_normalizes_reason_and_can_be_cleared() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let journal = store(py, &a2a, case.root());
        record(
            &journal,
            "held-message",
            "retain this",
            Some(coordination("thread-1", "Bounded status", "open", vec![])),
        );
        journal
            .call_method1(
                "set_hold",
                ("held-message", "  awaiting\n independent   review "),
            )
            .unwrap();
        let held = sql_rows(
            &journal,
            "SELECT hold_reason FROM message_state WHERE message_id='held-message'",
        );
        assert_eq!(held.len(), 1);
        assert_eq!(held[0]["hold_reason"], "awaiting independent review");
        journal
            .call_method1("set_hold", ("held-message", py.None()))
            .unwrap();
        let cleared = sql_rows(
            &journal,
            "SELECT hold_reason FROM message_state WHERE message_id='held-message'",
        );
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0]["hold_reason"].is_null());
    });
}

#[test]
fn watch_once_does_not_present_the_same_message_twice() {
    let case = Case::new();
    Python::attach(|py| {
        let a2a = module(py, "conductor.agent_a2a");
        let journal = store(py, &a2a, case.root());
        record(
            &journal,
            "watch-once",
            "watch body",
            Some(coordination("thread-1", "Watch summary", "open", vec![])),
        );
        let (stdout, _patch) = capture(py, "stdout");
        let command = main_args(
            case.root(),
            &["watch", "--as-name", "tester-a", "--once", "--json"],
        );
        run_main(&a2a, command.clone());
        let first: Value = serde_json::from_str(&buffer_text(&stdout)).unwrap();
        let ids = first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["watch-once"]);
        clear_buffer(&stdout);
        run_main(&a2a, command);
        assert_eq!(buffer_text(&stdout), "");
        assert_eq!(rows(&journal, true).as_array().unwrap().len(), 1);
    });
}
