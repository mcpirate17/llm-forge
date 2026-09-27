#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for the production stdio MCP probe.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/crg_mcp_probe_support.rs"]
mod probe_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use probe_support::{call_probe, probe_error, probe_error_parts, probe_module, CHILD, CHILD_ARG};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};
use serde_json::json;
use support::{path, Case};

#[test]
fn probe_completes_the_handshake_and_reports_the_listed_tools() {
    let case = Case::new();
    Python::attach(|py| {
        let report = call_probe(py, case.root(), &[("FAKE_TOOLS", "4")], Some(4)).unwrap();
        let tools = PyList::new(py, ["tool_0", "tool_1", "tool_2", "tool_3"]).unwrap();
        assert!(report.get_item("tools").unwrap().eq(&tools).unwrap());
        assert!(report.get_item("call").unwrap().eq("stats_tool").unwrap());
    });
}

#[test]
fn probe_fails_when_the_tool_count_differs_from_the_expected_one() {
    let case = Case::new();
    Python::attach(|py| {
        probe_error(
            py,
            call_probe(py, case.root(), &[], Some(22)),
            "expected 22 tools, server listed 3",
        );
    });
}

#[test]
fn probe_fails_when_a_result_string_carries_an_absolute_path() {
    let case = Case::new();
    Python::attach(|py| {
        probe_error(
            py,
            call_probe(
                py,
                case.root(),
                &[("FAKE_LEAK", "/home/tim/Projects/LLM/conductor/gate.py:12")],
                Some(3),
            ),
            "absolute paths in results",
        );
        let clean = call_probe(
            py,
            case.root(),
            &[("FAKE_LEAK", "conductor/gate.py:12")],
            None,
        )
        .unwrap();
        assert!(clean
            .get_item("needles")
            .unwrap()
            .eq(PyList::new(py, ["/home/"]).unwrap())
            .unwrap());
    });
}

#[test]
fn probe_surfaces_a_jsonrpc_error_from_the_tool_call() {
    let case = Case::new();
    Python::attach(|py| {
        probe_error_parts(
            py,
            call_probe(py, case.root(), &[("FAKE_CALL_ERROR", "1")], None),
            &["tools/call: ", "boom"],
        );
    });
}

#[test]
fn find_absolute_paths_scans_keys_and_nested_values() {
    let _case = Case::new();
    Python::attach(|py| {
        let probe = probe_module(py);
        let result = py_json(
            py,
            json!({
                "content":[{"type":"text","text":"see /home/tim/x.py"}],
                "meta":{"/home/tim/root":["nested",{"deep":"/srv/other"}]}
            }),
        );
        let hits = probe
            .getattr("find_absolute_paths")
            .unwrap()
            .call1((result, vec!["/home/"]))
            .unwrap();
        assert_eq!(hits.len().unwrap(), 2);
        let clean = probe
            .getattr("find_absolute_paths")
            .unwrap()
            .call1((py_json(py, json!({"a":["relative/x.py"]})), vec!["/home/"]))
            .unwrap();
        assert!(clean.eq(PyList::empty(py)).unwrap());
        let walked = probe
            .getattr("walk_strings")
            .unwrap()
            .call1((py_json(py, json!({"k":[1,null,"v"]})),))
            .unwrap();
        assert!(walked.eq(PyList::new(py, ["k", "v"]).unwrap()).unwrap());
    });
}

#[test]
fn load_server_cmd_reads_the_declared_command_args_cwd_and_env() {
    let case = Case::new();
    let source = json!({"mcpServers":{"code-review-graph":{
        "command":"/usr/bin/python3",
        "args":["-m","conductor.crg_server","--repo",case.root().to_str().unwrap()],
        "cwd":case.root().to_str().unwrap(),
        "env":{"CRG_ROLE":"review"}
    }}});
    case.write(".mcp.json", &source.to_string());
    Python::attach(|py| {
        let probe = probe_module(py);
        let result = probe
            .getattr("load_server_cmd")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let tuple = result.cast::<PyTuple>().unwrap();
        let expected = PyList::new(
            py,
            [
                "/usr/bin/python3",
                "-m",
                "conductor.crg_server",
                "--repo",
                case.root().to_str().unwrap(),
            ],
        )
        .unwrap();
        assert!(tuple.get_item(0).unwrap().eq(expected).unwrap());
        assert!(tuple
            .get_item(1)
            .unwrap()
            .eq(path(py, case.root()))
            .unwrap());
        assert!(tuple
            .get_item(2)
            .unwrap()
            .eq(py_json(py, json!({"CRG_ROLE":"review"})))
            .unwrap());
        probe_error_parts(
            py,
            probe
                .getattr("load_server_cmd")
                .unwrap()
                .call1((path(py, &case.root().join("missing")),)),
            &["no ", ".mcp.json"],
        );
    });
}

fn main_args(case: &Case, expected_tools: &str, call: bool) -> Vec<String> {
    let mut args = vec![
        "--server-cmd".to_owned(),
        CHILD.to_owned(),
        CHILD_ARG.to_owned(),
        "--repo".to_owned(),
        case.root().to_str().unwrap().to_owned(),
        "--expect-tools".to_owned(),
        expected_tools.to_owned(),
    ];
    if call {
        args.extend([
            "--call".to_owned(),
            "stats_tool".to_owned(),
            "{}".to_owned(),
        ]);
    }
    args
}

#[test]
fn main_prints_a_pass_verdict_and_exits_zero() {
    let case = Case::new();
    Python::attach(|py| {
        let (out, _stdout) = capture(py, "stdout");
        let (_err, _stderr) = capture(py, "stderr");
        let code: i32 = probe_module(py)
            .getattr("main")
            .unwrap()
            .call1((main_args(&case, "3", true),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            buffer_text(&out).trim(),
            "crg-probe | PASS 3 tools, call=stats_tool, no absolute paths in results"
        );
    });
}

#[test]
fn main_prints_a_fail_verdict_and_exits_nonzero() {
    let case = Case::new();
    Python::attach(|py| {
        let (_out, _stdout) = capture(py, "stdout");
        let (err, _stderr) = capture(py, "stderr");
        let code: i32 = probe_module(py)
            .getattr("main")
            .unwrap()
            .call1((main_args(&case, "9", false),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
        assert!(buffer_text(&err).starts_with("crg-probe | FAIL expected 9 tools"));
    });
}
