#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for CRG response compaction and tool wrapping.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/crg_response_support.rs"]
mod response_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, py_json, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use response_support::{compact, exact_json_value, fake_mcp, namespace, node, shim, ABS, REPO};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use support::{assert_error, module, path, AttrPatch, Case};

fn node_with(mut updates: Value) -> Value {
    let mut base = node();
    base.as_object_mut()
        .unwrap()
        .append(updates.as_object_mut().unwrap());
    base
}

fn assert_shim_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    assert_error(
        py,
        result.unwrap_err(),
        &shim(py).getattr("ResponseShimError").unwrap(),
        part,
    );
}

fn disable_pin(py: Python<'_>) -> AttrPatch {
    let no_args = signature(py, &[], &[]);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        bind_signature(&no_args, args, kwargs)?;
        Ok(())
    })
    .unwrap();
    AttrPatch::replace(
        shim(py).as_any(),
        "assert_supported_fastmcp",
        callback.as_any(),
    )
}

fn no_arg_tool(py: Python<'_>, result: Value) -> Bound<'_, PyCFunction> {
    let expected = signature(py, &[], &[]);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        bind_signature(&expected, args, kwargs)?;
        Ok(py_json(args.py(), result.clone()).unbind())
    })
    .unwrap()
}

fn tool<'py>(py: Python<'py>, fn_: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    namespace(py, &[("fn", fn_)])
}

fn frozen<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    module(py, "builtins")
        .getattr("frozenset")
        .unwrap()
        .call1((values,))
        .unwrap()
}

fn hidden_set(py: Python<'_>) -> BTreeSet<String> {
    shim(py)
        .getattr("hidden_tool_names")
        .unwrap()
        .call0()
        .unwrap()
        .extract()
        .unwrap()
}

fn default_hidden_set(py: Python<'_>) -> BTreeSet<String> {
    shim(py)
        .getattr("DEFAULT_HIDDEN_TOOLS")
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn compact_relativizes_and_elides_derivable_fields() {
    let _case = Case::new();
    Python::attach(|py| {
        let output = compact(py, json!({"results": [node()]}), Some(false), None);
        assert_eq!(
            output["results"][0],
            json!({
                "kind": "Function", "qualified_name": "pkg/mod.py::compact_state",
                "line_start": 77, "line_end": 104,
            })
        );
    });
}

#[test]
fn compact_keeps_name_when_not_derivable_and_true_is_test() {
    let _case = Case::new();
    Python::attach(|py| {
        let output = compact(
            py,
            node_with(json!({"name":"Other","is_test":true,"parent_name":"Cls"})),
            Some(false),
            None,
        );
        assert_eq!(output["name"], "Other");
        assert_eq!(output["is_test"], true);
        assert_eq!(output["parent_name"], "Cls");
        assert!(output.get("file_path").is_none());
    });
}

#[test]
fn compact_keeps_file_path_when_no_carrier_matches() {
    let _case = Case::new();
    Python::attach(|py| {
        let edge = json!({
            "id":1,"kind":"CALLS","source":"/repo/root/a.py::f",
            "target":"/repo/root/b.py::g","file_path":"/repo/root/c.py","line":3,
        });
        assert_eq!(
            compact(py, edge, Some(false), None),
            json!({
                "kind":"CALLS","source":"a.py::f","target":"b.py::g",
                "file_path":"c.py","line":3,
            })
        );
    });
}

#[test]
fn file_node_collapses_to_qualified_name_only() {
    let _case = Case::new();
    Python::attach(|py| {
        let file = node_with(
            json!({"kind":"File","name":ABS,"qualified_name":ABS,"line_start":1,"line_end":9}),
        );
        assert_eq!(
            compact(py, file, Some(false), None),
            json!({
                "kind":"File","qualified_name":"pkg/mod.py","line_start":1,"line_end":9,
            })
        );
    });
}

#[test]
fn hints_dropped_by_default_and_kept_on_request() {
    let mut case = Case::new();
    let payload = json!({"status":"ok","_hints":{"next_steps":[]}});
    Python::attach(|py| {
        let hints: String = shim(py).getattr("HINTS_ENV").unwrap().extract().unwrap();
        assert_eq!(hints, "CRG_KEEP_HINTS");
    });
    case.remove_env("CRG_KEEP_HINTS");
    Python::attach(|py| {
        assert!(compact(py, payload.clone(), None, None)
            .get("_hints")
            .is_none())
    });
    case.set_env("CRG_KEEP_HINTS", "1");
    Python::attach(|py| {
        assert_eq!(
            compact(py, payload.clone(), None, None)["_hints"],
            json!({"next_steps":[]})
        );
        assert!(compact(py, payload, Some(false), None)
            .get("_hints")
            .is_none());
    });
}

#[test]
fn long_lists_are_cut_with_explicit_marker() {
    let mut case = Case::new();
    let payload =
        json!({"results": (0..7).map(|n| json!({"kind":"Function","n":n})).collect::<Vec<_>>()});
    Python::attach(|py| {
        let output = compact(py, payload.clone(), Some(false), Some(5));
        assert_eq!(
            output["results"]
                .as_array()
                .unwrap()
                .iter()
                .take(5)
                .map(|row| row["n"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        let key: String = shim(py)
            .getattr("TRUNCATED_KEY")
            .unwrap()
            .extract()
            .unwrap();
        let marker = serde_json::Map::from_iter([(key, json!(2))]);
        assert_eq!(output["results"][5], Value::Object(marker));
        assert_eq!(output["results"].as_array().unwrap().len(), 6);
        assert_eq!(
            compact(py, payload.clone(), Some(false), Some(7))["results"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
    });
    case.set_env("CRG_MAX_LIST_ITEMS", "3");
    Python::attach(|py| {
        let key: String = shim(py)
            .getattr("TRUNCATED_KEY")
            .unwrap()
            .extract()
            .unwrap();
        let marker = serde_json::Map::from_iter([(key, json!(4))]);
        assert_eq!(
            compact(py, payload.clone(), Some(false), None)["results"][3],
            Value::Object(marker)
        );
    });
    case.set_env("CRG_MAX_LIST_ITEMS", "0");
    Python::attach(|py| {
        let options = PyDict::new(py);
        options.set_item("keep_hints", false).unwrap();
        assert_shim_error(
            py,
            shim(py).getattr("compact_payload").unwrap().call(
                (py_json(py, payload), path(py, Path::new(REPO))),
                Some(&options),
            ),
            ">= 1",
        );
    });
    case.remove_env("CRG_MAX_LIST_ITEMS");
    Python::attach(|py| {
        let shim = shim(py);
        assert!(shim
            .getattr("_max_list_items")
            .unwrap()
            .call0()
            .unwrap()
            .eq(shim.getattr("DEFAULT_MAX_LIST_ITEMS").unwrap())
            .unwrap());
    });
}

#[test]
fn relativize_prefix_property() {
    let _case = Case::new();
    Python::attach(|py| {
        for (value, expected) in [
            ("/repo/root/a.py", "a.py"),
            ("/repo/root/", ""),
            ("/repo/root", "/repo/root"),
            ("/repo/rootless/a.py", "/repo/rootless/a.py"),
            ("relative/a.py", "relative/a.py"),
            ("", ""),
        ] {
            let output = compact(py, json!(value), Some(false), None);
            assert_eq!(output, expected);
            assert_eq!(compact(py, output.clone(), Some(false), None), output);
            assert!(output.as_str().unwrap().len() <= value.len());
        }
    });
}

#[test]
fn list_cap_property() {
    let _case = Case::new();
    Python::attach(|py| {
        let key: String = shim(py)
            .getattr("TRUNCATED_KEY")
            .unwrap()
            .extract()
            .unwrap();
        let payload = json!((0..6)
            .map(|n| json!({"kind":"F","n":n}))
            .collect::<Vec<_>>());
        for maximum in [1, 2, 3, 5, 8] {
            let output = compact(py, payload.clone(), Some(false), Some(maximum));
            let rows = output.as_array().unwrap();
            let kept = rows.iter().filter(|row| row.get(&key).is_none()).count();
            let dropped: usize = rows
                .iter()
                .map(|row| row.get(&key).and_then(Value::as_u64).unwrap_or(0) as usize)
                .sum();
            assert_eq!(kept, 6.min(maximum));
            assert_eq!(kept + dropped, 6);
        }
    });
}

#[test]
fn non_json_payloads_pass_through() {
    let _case = Case::new();
    Python::attach(|py| {
        for value in [json!("plain text"), json!(7), Value::Null] {
            assert_eq!(compact(py, value.clone(), Some(false), None), value);
        }
    });
}

#[test]
fn prefix_only_strips_repo_root_not_lookalikes() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            compact(
                py,
                json!({"a":"/repo/rootless/x.py","b":"/repo/root/x.py"}),
                Some(false),
                None
            ),
            json!({"a":"/repo/rootless/x.py","b":"x.py"})
        );
    });
}

#[test]
fn install_wraps_sync_and_async_tools() {
    let mut case = Case::new();
    case.remove_env("CRG_KEEP_HINTS");
    Python::attach(|py| {
        let _pin = disable_pin(py);
        let sync = no_arg_tool(py, json!({"file_path":ABS,"id":1,"_hints":{}}));
        let async_body = PyCFunction::new_closure(py, None, None, {
            let expected = signature(py, &[], &[]);
            move |args, kwargs| -> PyResult<Py<PyAny>> {
                bind_signature(&expected, args, kwargs)?;
                let options = PyDict::new(args.py());
                options.set_item(
                    "result",
                    py_json(args.py(), json!({"results":[node()],"_hints":{}})),
                )?;
                Ok(module(args.py(), "asyncio")
                    .getattr("sleep")?
                    .call((0,), Some(&options))?
                    .unbind())
            }
        })
        .unwrap();
        let mock_kwargs = PyDict::new(py);
        mock_kwargs.set_item("wraps", &async_body).unwrap();
        let async_mock = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&mock_kwargs))
            .unwrap();
        module(py, "inspect")
            .getattr("markcoroutinefunction")
            .unwrap()
            .call1((&async_mock,))
            .unwrap();
        let sync_tool = tool(py, sync.as_any());
        let async_tool = tool(py, &async_mock);
        let (mcp, _) = fake_mcp(
            py,
            &[("sync", sync_tool.clone()), ("async", async_tool.clone())],
        );
        assert!(shim(py)
            .getattr("install_response_shim")
            .unwrap()
            .call1((&mcp, path(py, Path::new(REPO))))
            .unwrap()
            .eq(2)
            .unwrap());
        assert_eq!(
            exact_json_value(&sync_tool.getattr("fn").unwrap().call0().unwrap()),
            json!({"file_path":"pkg/mod.py"})
        );
        let coroutine = async_tool.getattr("fn").unwrap().call0().unwrap();
        let output = module(py, "asyncio")
            .getattr("run")
            .unwrap()
            .call1((coroutine,))
            .unwrap();
        assert_eq!(
            exact_json_value(&output),
            json!({"results":[{
                "kind":"Function","qualified_name":"pkg/mod.py::compact_state",
                "line_start":77,"line_end":104
            }]})
        );
        module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call1((output,))
            .unwrap();
    });
}

#[test]
fn install_applies_enrichers_before_compaction() {
    let mut case = Case::new();
    case.remove_env("CRG_KEEP_HINTS");
    Python::attach(|py| {
        let _pin = disable_pin(py);
        let search = tool(
            py,
            no_arg_tool(py, json!({"results":[node()],"_hints":{}})).as_any(),
        );
        let other = tool(py, no_arg_tool(py, json!({"x":ABS})).as_any());
        let (mcp, _) = fake_mcp(py, &[("search", search.clone()), ("other", other.clone())]);
        let expected = signature(py, &["payload"], &[]);
        let enrich =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let payload = bound.getattr("arguments")?.get_item("payload")?;
                let hits = payload.get_item("results")?;
                for hit in hits.try_iter()? {
                    let hit = hit?;
                    let source: String = hit.get_item("file_path")?.extract()?;
                    hit.set_item("doc", format!("seen {source}"))?;
                }
                Ok(payload.unbind())
            })
            .unwrap();
        let enrichers = PyDict::new(py);
        enrichers.set_item("search", &enrich).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("enrichers", &enrichers).unwrap();
        shim(py)
            .getattr("install_response_shim")
            .unwrap()
            .call((&mcp, path(py, Path::new(REPO))), Some(&kwargs))
            .unwrap();
        let search_output = exact_json_value(&search.getattr("fn").unwrap().call0().unwrap());
        assert_eq!(search_output["results"][0]["doc"], format!("seen {ABS}"));
        assert_eq!(
            search_output["results"][0]["qualified_name"],
            "pkg/mod.py::compact_state"
        );
        assert_eq!(
            exact_json_value(&other.getattr("fn").unwrap().call0().unwrap()),
            json!({"x":"pkg/mod.py"})
        );
        let unknown = PyDict::new(py);
        unknown.set_item("nope", &enrich).unwrap();
        kwargs.set_item("enrichers", unknown).unwrap();
        assert_shim_error(
            py,
            shim(py)
                .getattr("install_response_shim")
                .unwrap()
                .call((&mcp, path(py, Path::new(REPO))), Some(&kwargs)),
            "unknown tools: ['nope']",
        );
    });
}

#[test]
fn install_fails_loud_when_seam_is_missing() {
    let _case = Case::new();
    Python::attach(|py| {
        let _pin = disable_pin(py);
        let empty = namespace(py, &[]);
        assert_shim_error(
            py,
            shim(py)
                .getattr("install_response_shim")
                .unwrap()
                .call1((empty, path(py, Path::new(REPO)))),
            "_local_provider",
        );
        let no_fn = namespace(py, &[("fn", py.None().bind(py))]);
        let (mcp, _) = fake_mcp(py, &[("t", no_fn)]);
        assert_shim_error(
            py,
            shim(py)
                .getattr("install_response_shim")
                .unwrap()
                .call1((mcp, path(py, Path::new(REPO)))),
            "no callable fn",
        );
    });
}

#[test]
fn registered_tools_skips_non_tool_components_and_needs_one_tool() {
    let _case = Case::new();
    Python::attach(|py| {
        let only = tool(py, no_arg_tool(py, Value::Null).as_any());
        let (mcp, _) = fake_mcp(py, &[("only", only.clone())]);
        let found = shim(py)
            .getattr("registered_tools")
            .unwrap()
            .call1((mcp,))
            .unwrap();
        let expected = PyDict::new(py);
        expected.set_item("only", only).unwrap();
        assert!(found.eq(expected).unwrap());
        let (empty, _) = fake_mcp(py, &[]);
        assert_shim_error(
            py,
            shim(py)
                .getattr("registered_tools")
                .unwrap()
                .call1((empty,)),
            "holds no tools",
        );
    });
}

#[test]
fn hidden_tool_names_env_override() {
    let mut case = Case::new();
    case.remove_env("CRG_HIDDEN_TOOLS");
    case.remove_env("CRG_ROLE");
    Python::attach(|py| {
        let shim = shim(py);
        assert!(shim
            .getattr("hidden_tool_names")
            .unwrap()
            .call0()
            .unwrap()
            .eq(shim.getattr("DEFAULT_HIDDEN_TOOLS").unwrap())
            .unwrap());
    });
    case.set_env("CRG_HIDDEN_TOOLS", " a_tool, b_tool ,");
    Python::attach(|py| {
        assert_eq!(
            hidden_set(py),
            BTreeSet::from(["a_tool".to_owned(), "b_tool".to_owned()])
        )
    });
    case.set_env("CRG_HIDDEN_TOOLS", "");
    Python::attach(|py| {
        assert!(shim(py)
            .getattr("hidden_tool_names")
            .unwrap()
            .call0()
            .unwrap()
            .eq(frozen(py, &[]))
            .unwrap())
    });
    case.remove_env("CRG_HIDDEN_TOOLS");
    case.set_env("CRG_ROLE", "static");
    Python::attach(|py| {
        let hidden = hidden_set(py);
        let default = default_hidden_set(py);
        assert!(default.is_subset(&hidden) && default != hidden);
        assert!(hidden.contains("get_impact_radius_tool"));
        assert!(
            !hidden.contains("query_graph_tool") && !hidden.contains("semantic_search_nodes_tool")
        );
    });
    case.set_env("CRG_ROLE", "review");
    Python::attach(|py| {
        let hidden = hidden_set(py);
        let default = default_hidden_set(py);
        assert_eq!(
            hidden,
            default
                .union(&BTreeSet::from([
                    "refactor_tool".to_owned(),
                    "apply_refactor_tool".to_owned()
                ]))
                .cloned()
                .collect()
        );
    });
    case.set_env("CRG_ROLE", "wizard");
    Python::attach(|py| {
        assert_shim_error(
            py,
            shim(py).getattr("hidden_tool_names").unwrap().call0(),
            "wizard",
        )
    });
    case.set_env("CRG_HIDDEN_TOOLS", "x_tool");
    Python::attach(|py| {
        assert!(shim(py)
            .getattr("hidden_tool_names")
            .unwrap()
            .call0()
            .unwrap()
            .eq(frozen(py, &["x_tool"]))
            .unwrap())
    });
}

#[test]
fn prune_tools_removes_only_listed_and_fails_on_unknown() {
    let _case = Case::new();
    Python::attach(|py| {
        let fn_ = no_arg_tool(py, Value::Null);
        let tools = ["keep", "drop_a", "drop_b"].map(|name| (name, tool(py, fn_.as_any())));
        let (mcp, provider) = fake_mcp(py, &tools);
        let removed = PyList::empty(py);
        provider
            .setattr("remove_tool", removed.getattr("append").unwrap())
            .unwrap();
        let hidden = frozen(py, &["drop_b", "drop_a"]);
        let output = shim(py)
            .getattr("prune_tools")
            .unwrap()
            .call1((&mcp, hidden))
            .unwrap();
        let expected = PyList::new(py, ["drop_a", "drop_b"]).unwrap();
        assert!(output.cast::<PyList>().is_ok());
        assert!(output.eq(&expected).unwrap());
        assert!(removed.eq(&expected).unwrap());
        assert_shim_error(
            py,
            shim(py)
                .getattr("prune_tools")
                .unwrap()
                .call1((&mcp, frozen(py, &["keep", "nope"]))),
            "unknown CRG tools: ['nope']",
        );
        let (without, _) = fake_mcp(py, &tools);
        assert_shim_error(
            py,
            shim(py)
                .getattr("prune_tools")
                .unwrap()
                .call1((without, frozen(py, &["keep"]))),
            "remove_tool",
        );
    });
}

#[test]
fn version_pin_rejects_drift() {
    let _case = Case::new();
    Python::attach(|py| {
        let shim_module = shim(py);
        let metadata = shim_module
            .getattr("importlib")
            .unwrap()
            .getattr("metadata")
            .unwrap();
        let expected = signature(py, &["_name"], &[]);
        let wrong =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<&str> {
                bind_signature(&expected, args, kwargs)?;
                Ok("9.9.9")
            })
            .unwrap();
        let _wrong = AttrPatch::replace(&metadata, "version", wrong.as_any());
        assert_shim_error(
            py,
            shim_module
                .getattr("assert_supported_fastmcp")
                .unwrap()
                .call0(),
            "9.9.9",
        );
        let expected = signature(py, &["_name"], &[]);
        let right =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                bind_signature(&expected, args, kwargs)?;
                Ok(shim(args.py())
                    .getattr("EXPECTED_FASTMCP_VERSION")?
                    .unbind())
            })
            .unwrap();
        let _right = AttrPatch::replace(&metadata, "version", right.as_any());
        shim_module
            .getattr("assert_supported_fastmcp")
            .unwrap()
            .call0()
            .unwrap();
    });
}
