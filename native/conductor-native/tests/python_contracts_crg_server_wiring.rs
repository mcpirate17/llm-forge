#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for the CRG server startup wiring.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/crg_server_support.rs"]
#[allow(dead_code)]
mod server_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use server_support::{append_record, fake_main, signature_optional_none, ModulePatch};
use std::path::Path;
use support::{module, path, AttrPatch, Case};

struct Wired<'py> {
    server: Bound<'py, PyModule>,
    calls: Bound<'py, PyList>,
    _attributes: Vec<AttrPatch>,
    _modules: Vec<ModulePatch>,
}

fn record_none<'py>(
    py: Python<'py>,
    calls: &Bound<'py, PyList>,
    name: &str,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &[], &[]);
    let calls = calls.clone().unbind();
    let name = name.to_owned();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        bind_signature(&expected, args, kwargs)?;
        append_record(
            args.py(),
            calls.bind(args.py()),
            &name,
            args.py().None().bind(args.py()),
        )
    })
    .unwrap()
}

fn record_arg<'py>(
    py: Python<'py>,
    calls: &Bound<'py, PyList>,
    event: &str,
    parameter: &str,
    fake_mcp: Option<&Bound<'py, PyAny>>,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &[parameter], &[]);
    let calls = calls.clone().unbind();
    let event = event.to_owned();
    let parameter = parameter.to_owned();
    let fake_mcp = fake_mcp.cloned().map(Bound::unbind);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        let bound = bind_signature(&expected, args, kwargs)?;
        let value = bound.getattr("arguments")?.get_item(&parameter)?;
        let actual = match &fake_mcp {
            Some(mcp) => PyBool::new(args.py(), value.is(mcp.bind(args.py())))
                .as_any()
                .clone(),
            None => value,
        };
        append_record(args.py(), calls.bind(args.py()), &event, &actual)
    })
    .unwrap()
}

fn enricher<'py>(py: Python<'py>) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["root"], &[]);
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Py<PyDict>> {
            let bound = bind_signature(&expected, args, kwargs)?;
            let root = bound.getattr("arguments")?.get_item("root")?;
            let result = PyDict::new(args.py());
            result.set_item("enrich", root)?;
            Ok(result.unbind())
        },
    )
    .unwrap()
}

fn record_shim<'py>(
    py: Python<'py>,
    calls: &Bound<'py, PyList>,
    mcp: &Bound<'py, PyAny>,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["mcp", "root", "enrichers"], &[]);
    let calls = calls.clone().unbind();
    let mcp = mcp.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        let bound = bind_signature(&expected, args, kwargs)?;
        let arguments = bound.getattr("arguments")?;
        let passed = arguments.get_item("mcp")?;
        let root = arguments.get_item("root")?;
        let enrichers = arguments.get_item("enrichers")?;
        let same = PyBool::new(args.py(), passed.is(mcp.bind(args.py())));
        let value = PyTuple::new(args.py(), [same.as_any(), &root, &enrichers])?;
        append_record(args.py(), calls.bind(args.py()), "shim", value.as_any())
    })
    .unwrap()
}

fn crg_main<'py>(py: Python<'py>, calls: &Bound<'py, PyList>) -> Bound<'py, PyCFunction> {
    let expected = signature_optional_none(py, "repo_root");
    let calls = calls.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        let bound = expected.bind(args.py()).call_method("bind", args, kwargs)?;
        bound.call_method0("apply_defaults")?;
        let value = bound.getattr("arguments")?.get_item("repo_root")?;
        append_record(args.py(), calls.bind(args.py()), "crg_main", &value)
    })
    .unwrap()
}

fn wired(py: Python<'_>) -> Wired<'_> {
    let server = module(py, "conductor.crg_server");
    let calls = PyList::empty(py);
    let fake_mcp = module(py, "builtins")
        .getattr("object")
        .unwrap()
        .call0()
        .unwrap();
    let (main, modules) = fake_main(py);
    main.add("mcp", &fake_mcp).unwrap();
    main.add("main", crg_main(py, &calls)).unwrap();
    let replacements = [
        ("install_bridge", record_none(py, &calls, "bridge")),
        (
            "install_node_text",
            record_arg(py, &calls, "node_text", "root", None),
        ),
        (
            "register_workspace_tools",
            record_arg(py, &calls, "register", "mcp", Some(&fake_mcp)),
        ),
        (
            "assert_supported_fastmcp",
            record_none(py, &calls, "fastmcp_pin"),
        ),
        (
            "prune_tools",
            record_arg(py, &calls, "prune", "mcp", Some(&fake_mcp)),
        ),
        ("search_enrichers", enricher(py)),
        ("install_response_shim", record_shim(py, &calls, &fake_mcp)),
    ];
    let attributes = replacements
        .into_iter()
        .map(|(name, callback)| AttrPatch::replace(server.as_any(), name, callback.as_any()))
        .collect();
    Wired {
        server,
        calls,
        _attributes: attributes,
        _modules: modules,
    }
}

fn expected_record<'py>(
    py: Python<'py>,
    name: &str,
    value: &Bound<'py, PyAny>,
) -> Bound<'py, PyTuple> {
    PyTuple::new(py, [pyo3::types::PyString::new(py, name).as_any(), value]).unwrap()
}

fn assert_records(actual: &Bound<'_, PyList>, expected: &Bound<'_, PyList>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual: {}",
        actual.repr().unwrap()
    );
}

#[test]
fn main_wires_everything_in_order() {
    let mut case = Case::new();
    case.remove_env("CRG_SHIM_DISABLE");
    Python::attach(|py| {
        let fixture = wired(py);
        assert_eq!(
            fixture
                .server
                .getattr("main")
                .unwrap()
                .call1((vec!["--repo", "/r"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let root = path(py, Path::new("/r"));
        let enrich = PyDict::new(py);
        enrich.set_item("enrich", &root).unwrap();
        let shim =
            PyTuple::new(py, [PyBool::new(py, true).as_any(), &root, enrich.as_any()]).unwrap();
        let expected = PyList::empty(py);
        for (name, value) in [
            ("bridge", py.None()),
            ("node_text", root.clone().unbind()),
            ("register", PyBool::new(py, true).as_any().clone().unbind()),
            ("fastmcp_pin", py.None()),
            ("prune", PyBool::new(py, true).as_any().clone().unbind()),
            ("shim", shim.into_any().unbind()),
            (
                "crg_main",
                "/r".into_pyobject(py).unwrap().into_any().unbind(),
            ),
        ] {
            expected
                .append(expected_record(py, name, value.bind(py)))
                .unwrap();
        }
        assert_records(&fixture.calls, &expected);
    });
}

#[test]
fn shim_disable_skips_workspace_wiring_but_keeps_node_text() {
    let mut case = Case::new();
    case.set_env("CRG_SHIM_DISABLE", "1");
    Python::attach(|py| {
        let fixture = wired(py);
        assert_eq!(
            fixture
                .server
                .getattr("main")
                .unwrap()
                .call1((Vec::<String>::new(),))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let names: Vec<String> = fixture
            .calls
            .iter()
            .map(|row| row.get_item(0).unwrap().extract().unwrap())
            .collect();
        assert_eq!(names, ["bridge", "node_text", "crg_main"]);
        let root = fixture.server.getattr("ROOT").unwrap();
        let actual = fixture.calls.get_item(1).unwrap().get_item(1).unwrap();
        assert!(actual.eq(&root).unwrap());
    });
}

#[test]
fn bridge_error_invariant_fails_loud() {
    let _case = Case::new();
    Python::attach(|py| {
        let fixture = wired(py);
        let error = module(py, "conductor.crg_embedding_bridge")
            .getattr("CrgBridgeError")
            .unwrap()
            .unbind();
        let expected = signature(py, &[], &[]);
        let boom = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
            bind_signature(&expected, args, kwargs)?;
            Err(PyErr::from_value(
                error
                    .bind(args.py())
                    .call1(("version drift",))?
                    .as_any()
                    .clone(),
            ))
        })
        .unwrap();
        let _patch = AttrPatch::replace(fixture.server.as_any(), "install_bridge", boom.as_any());
        let result = fixture
            .server
            .getattr("main")
            .unwrap()
            .call1((Vec::<String>::new(),));
        assert!(result
            .unwrap_err()
            .is_instance_of::<pyo3::exceptions::PySystemExit>(py));
        assert!(fixture.calls.is_empty());
    });
}

#[test]
fn shim_errors_fail_loud() {
    let mut case = Case::new();
    case.remove_env("CRG_SHIM_DISABLE");
    Python::attach(|py| {
        let fixture = wired(py);
        let error = module(py, "conductor.crg_response_shim")
            .getattr("ResponseShimError")
            .unwrap()
            .unbind();
        let expected = signature(py, &["mcp"], &[]);
        let boom = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
            bind_signature(&expected, args, kwargs)?;
            Err(PyErr::from_value(
                error
                    .bind(args.py())
                    .call1(("seam moved",))?
                    .as_any()
                    .clone(),
            ))
        })
        .unwrap();
        let _patch = AttrPatch::replace(fixture.server.as_any(), "prune_tools", boom.as_any());
        let result = fixture
            .server
            .getattr("main")
            .unwrap()
            .call1((Vec::<String>::new(),));
        assert!(result
            .unwrap_err()
            .is_instance_of::<pyo3::exceptions::PySystemExit>(py));
        for record in fixture.calls.iter() {
            assert_ne!(
                record.get_item(0).unwrap().extract::<String>().unwrap(),
                "crg_main"
            );
        }
    });
}
