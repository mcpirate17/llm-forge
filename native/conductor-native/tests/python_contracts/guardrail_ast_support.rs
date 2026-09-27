//! Rust-owned CPython-AST reference and policy fixtures for guardrail contracts.

use crate::ast_ref::{self, attr_str, attr_usize, children, is_any, is_kind, walk};
use crate::support::module;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList};
use serde_json::{json, Value};

fn control(node: &Bound<'_, PyAny>) -> bool {
    is_any(
        node,
        &[
            "If",
            "For",
            "AsyncFor",
            "While",
            "Try",
            "With",
            "AsyncWith",
            "Match",
        ],
    )
}

fn depth(node: &Bound<'_, PyAny>, current: usize) -> usize {
    let mut best = current;
    for child in children(node) {
        let next = current + usize::from(control(&child));
        best = best.max(depth(&child, next));
    }
    best
}

fn hot_loop(node: &Bound<'_, PyAny>) -> bool {
    for child in walk(node) {
        if !is_any(&child, &["For", "AsyncFor"]) {
            continue;
        }
        let nodes = walk(&child);
        let has_append = nodes
            .iter()
            .filter(|item| is_kind(item, "Call"))
            .any(|call| {
                let function = call.getattr("func").unwrap();
                is_kind(&function, "Attribute") && attr_str(&function, "attr") == "append"
            });
        let numeric = nodes
            .iter()
            .any(|item| is_any(item, &["BinOp", "AugAssign"]));
        if has_append && numeric {
            return true;
        }
        let iterator = child.getattr("iter").unwrap();
        if is_kind(&iterator, "Name")
            && ["x", "xs", "arr", "array", "tensor", "values"]
                .contains(&attr_str(&iterator, "id").as_str())
            && numeric
        {
            return true;
        }
    }
    false
}

fn collect_functions(node: &Bound<'_, PyAny>, functions: &mut Vec<Value>) {
    if is_any(node, &["FunctionDef", "AsyncFunctionDef"]) {
        let branches = walk(node)
            .iter()
            .filter(|child| {
                is_any(
                    child,
                    &["If", "For", "AsyncFor", "While", "Try", "Match", "IfExp"],
                )
            })
            .count();
        let name = attr_str(node, "name");
        let route = name.starts_with("register_")
            && children(node)
                .iter()
                .any(|child| is_any(child, &["FunctionDef", "AsyncFunctionDef"]));
        let ending = node.getattr("end_lineno").unwrap();
        let end: Option<usize> = if ending.is_none() {
            None
        } else {
            Some(ending.extract().unwrap())
        };
        functions.push(json!({
            "symbol":name,"lineno":attr_usize(node,"lineno"),"end_lineno":end,
            "branches":branches,"max_nesting":depth(node,0),
            "is_route_registration":route,"hot_loop":hot_loop(node)
        }));
    }
    for child in children(node) {
        collect_functions(&child, functions);
    }
}

pub fn reference_metrics(py: Python<'_>, path: &str, source: &str) -> Value {
    let tree = ast_ref::parse(py, source, path).unwrap();
    let mut functions = Vec::new();
    collect_functions(&tree, &mut functions);
    json!({"path":path,"parse_error":false,"functions":functions})
}

pub fn native_metrics(py: Python<'_>, records: &[(&str, &str)], policy: Option<&str>) -> Value {
    let py_records = PyList::new(py, records.iter().map(|&(path, source)| (path, source))).unwrap();
    let bridge = module(py, "conductor._native")
        .getattr("guardrail_ast_metrics_native")
        .unwrap();
    let payload: String = match policy {
        Some(policy) => bridge.call1((py_records, policy)).unwrap(),
        None => bridge.call1((py_records,)).unwrap(),
    }
    .extract()
    .unwrap();
    serde_json::from_str(&payload).unwrap()
}

pub fn native_issues(
    py: Python<'_>,
    path: &str,
    source: &str,
    allowlist: &Bound<'_, PyAny>,
) -> Vec<Value> {
    let sorted = |name: &str| -> Vec<String> {
        let entry = allowlist.get_item(name).unwrap();
        let mut values: Vec<String> = entry
            .try_iter()
            .unwrap()
            .map(|item| item.unwrap().extract().unwrap())
            .collect();
        values.sort();
        values
    };
    let policy = json!({"god_functions":sorted("god_functions"),"complexity":sorted("complexity")})
        .to_string();
    native_metrics(py, &[(path, source)], Some(&policy))[0]["issues"]
        .as_array()
        .unwrap()
        .clone()
}

pub fn allowlist<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let audit = module(py, "conductor.guardrail_audit");
    audit
        .getattr("_load_allowlist")
        .unwrap()
        .call1((audit.getattr("ROOT").unwrap(),))
        .unwrap()
}

pub fn kinds(issues: &[Value]) -> Vec<&str> {
    issues
        .iter()
        .map(|issue| issue["kind"].as_str().unwrap())
        .collect()
}

pub fn py_metric(py: Python<'_>, value: Value) -> Bound<'_, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}
