#![cfg(feature = "python-compat-tests")]
//! Rust-owned guardrail AST metrics and policy boundary contracts.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/guardrail_ast_support.rs"]
mod guard_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use guard_ref::{allowlist, kinds, native_issues, native_metrics, reference_metrics};
use pyo3::prelude::*;
use pyo3::types::PyList;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use support::{module, path, AttrPatch, Case};

const CONTROL: &str = r#"@decorate(flag if enabled else fallback)
def decorated(value=(left if choose else right)):
    if value:
        work()
    elif (other if choose else fallback):
        work()
    elif third:
        work()
    else:
        work()
    try:
        with resource():
            while ready():
                work()
    except Error:
        work()
    match value:
        case 1:
            work()
    return value
"#;

const NESTED: &str = r#"def outer(values):
    for value in values:
        total = value + 1
    def nested(items):
        for item in items:
            output.append(item * 2)
        return output
    return nested(values)

def numeric_values(values):
    for value in values:
        total = value + 1
    return total

def register_routes():
    async def handler(request):
        return request
    return handler

def register_indirect():
    if enabled:
        def handler(request):
            return request
    return enabled
"#;

const ASYNC_STAR: &str = r#"async def consume(stream):
    async for value in stream:
        total += value
    async with lock:
        if ready:
            return total

def exception_group():
    try:
        work()
    except* ValueError:
        recover()
"#;

#[test]
fn native_metrics_match_python_ast_reference_corpus() {
    let _case = Case::new();
    Python::attach(|py| {
        let records = [
            ("pkg/control.py", CONTROL),
            ("pkg/nested.py", NESTED),
            ("pkg/async_star.py", ASYNC_STAR),
        ];
        let actual = native_metrics(py, &records, None);
        let expected: Vec<_> = records
            .iter()
            .map(|&(path, source)| reference_metrics(py, path, source))
            .collect();
        assert_eq!(actual, Value::Array(expected));
    });
}

fn probe_source() -> String {
    let mut lines = vec![
        "def candidate(values):".to_owned(),
        "    # guardrail: allow-god-function".to_owned(),
    ];
    lines.extend((0..21).map(|index| format!("    if flag_{index}: pass")));
    lines.extend([
        "    for value in values:".to_owned(),
        "        output.append(value * 2)".to_owned(),
    ]);
    lines.extend((0..77).map(|_| "    value = 1".to_owned()));
    lines.join("\n")
}

fn route_source() -> String {
    let mut lines = vec![
        "def register_routes(values):".to_owned(),
        "    def handler():".to_owned(),
        "        return 1".to_owned(),
    ];
    lines.extend((0..21).map(|index| format!("    if flag_{index}: pass")));
    lines.extend([
        "    for value in values:".to_owned(),
        "        output.append(value * 2)".to_owned(),
    ]);
    lines.extend((0..77).map(|_| "    value = 1".to_owned()));
    lines.join("\n")
}

#[test]
fn policy_thresholds_markers_allowlists_and_order() {
    let _case = Case::new();
    Python::attach(|py| {
        let allowlist = allowlist(py);
        let source = probe_source();
        let issues = native_issues(py, "pkg/probe.py", &source, &allowlist);
        assert_eq!(kinds(&issues), ["complexity", "native_hotspot_candidate"]);
        assert_eq!(
            issues[0]["metric"],
            json!({"branches":22,"max_nesting":1,"lineno":1})
        );
        let mut boundary = vec!["def boundary():".to_owned()];
        boundary.extend((0..100).map(|_| "    value = 1".to_owned()));
        let threshold = native_issues(py, "pkg/boundary.py", &boundary.join("\n"), &allowlist);
        assert_eq!(kinds(&threshold), ["god_function"]);
        assert_eq!(threshold[0]["metric"], json!({"lines":101,"lineno":1}));
        let mut branch = vec!["def branchy():".to_owned()];
        branch.extend(
            (0..21).flat_map(|index| [format!("    if flag_{index}:"), "        pass".to_owned()]),
        );
        assert_eq!(
            kinds(&native_issues(
                py,
                "pkg/branch.py",
                &branch.join("\n"),
                &allowlist
            )),
            ["complexity"]
        );
        let mut nesting = vec!["def nested():".to_owned()];
        nesting.extend((0..6).map(|depth| format!("{}if flag_{depth}:", "    ".repeat(depth + 1))));
        nesting.push(format!("{}return 1", "    ".repeat(7)));
        assert_eq!(
            kinds(&native_issues(
                py,
                "pkg/nesting.py",
                &nesting.join("\n"),
                &allowlist
            )),
            ["complexity"]
        );
        allowlist
            .get_item("complexity")
            .unwrap()
            .call_method1("add", ("pkg/probe.py::candidate",))
            .unwrap();
        assert!(native_issues(py, "pkg/probe.py", &source, &allowlist).is_empty());
        for entry in allowlist
            .call_method0("values")
            .unwrap()
            .try_iter()
            .unwrap()
        {
            entry.unwrap().call_method0("clear").unwrap();
        }
        let route = native_issues(py, "pkg/routes.py", &route_source(), &allowlist);
        let selected: Vec<_> = route
            .iter()
            .filter(|issue| issue["symbol"] == "register_routes")
            .map(|issue| issue["kind"].as_str().unwrap())
            .collect();
        assert_eq!(selected, ["native_hotspot_candidate"]);
    });
}

#[test]
fn god_file_boundary_and_cpython_syntax_error() {
    let case = Case::new();
    Python::attach(|py| {
        let audit = module(py, "conductor.guardrail_audit");
        let _root = AttrPatch::replace(audit.as_any(), "ROOT", path(py, case.root()).as_any());
        let exact = case.root().join("exact.txt");
        let above = case.root().join("above.txt");
        let broken = case.root().join("broken.py");
        fs::write(&exact, vec!["line"; 1250].join("\n")).unwrap();
        fs::write(&above, vec!["line"; 1251].join("\n")).unwrap();
        fs::write(&broken, "value = 1\ndef broken(:\n").unwrap();
        let files =
            PyList::new(py, [path(py, &exact), path(py, &above), path(py, &broken)]).unwrap();
        let options = pyo3::types::PyDict::new(py);
        options.set_item("staged_only", false).unwrap();
        options.set_item("from_ref", py.None()).unwrap();
        let result = audit
            .getattr("_structural_issues")
            .unwrap()
            .call((files,), Some(&options))
            .unwrap();
        let issues = result.get_item(0).unwrap().cast_into::<PyList>().unwrap();
        assert!(result.get_item(1).unwrap().eq(1).unwrap());
        let pairs: Vec<(String, String)> = issues
            .iter()
            .map(|issue| {
                (
                    issue.getattr("kind").unwrap().extract().unwrap(),
                    issue.getattr("path").unwrap().extract().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("god_file".into(), "above.txt".into()),
                ("syntax_error".into(), "broken.py".into())
            ]
        );
        assert!(issues
            .get_item(0)
            .unwrap()
            .getattr("metric")
            .unwrap()
            .eq(guard_ref::py_metric(py, json!({"lines":1251})))
            .unwrap());
        assert!(issues
            .get_item(1)
            .unwrap()
            .getattr("message")
            .unwrap()
            .eq("Syntax error: invalid syntax")
            .unwrap());
        assert!(issues
            .get_item(1)
            .unwrap()
            .getattr("metric")
            .unwrap()
            .eq(guard_ref::py_metric(py, json!({"lineno":2})))
            .unwrap());
    });
}

const DIRECT_NESTED: &str = "def outer():\n    def nested():\n        if one:\n            if two:\n                if three:\n                    if four:\n                        if five:\n                            if six:\n                                return 1\n\ndef register_direct():\n    def handler():\n        return 1\n\ndef register_deep():\n    if enabled:\n        def handler():\n            return 1\n\ndef registerish():\n    def handler():\n        return 1\n";

#[test]
fn nested_function_and_direct_route_semantics_drive_policy() {
    let _case = Case::new();
    Python::attach(|py| {
        let metrics = native_metrics(py, &[("pkg/nested.py", DIRECT_NESTED)], None);
        let functions = metrics[0]["functions"].as_array().unwrap();
        let names: Vec<_> = functions
            .iter()
            .map(|function| function["symbol"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "outer",
                "nested",
                "register_direct",
                "handler",
                "register_deep",
                "handler",
                "registerish",
                "handler"
            ]
        );
        let by_name: BTreeMap<&str, &Value> = functions
            .iter()
            .map(|function| (function["symbol"].as_str().unwrap(), function))
            .collect();
        assert_eq!(by_name["outer"]["max_nesting"], 6);
        assert_eq!(by_name["nested"]["max_nesting"], 6);
        assert_eq!(by_name["register_direct"]["is_route_registration"], true);
        assert_eq!(by_name["register_deep"]["is_route_registration"], false);
        assert_eq!(by_name["registerish"]["is_route_registration"], false);
    });
}
