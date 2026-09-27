//! Rust-owned checkout and interpreter fixtures for CRG venv-sync contracts.

use crate::comm_support::{bind_signature, signature};
use crate::support::{assert_error, module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

pub const CHILD: &str = env!("CARGO_BIN_EXE_crg_venv_sync_child");
pub const VERSION: &str = "0.1.30";
pub const IMPORT_ERROR: &str = "cannot import name 'new_symbol_native'";

pub fn sync(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.crg_venv_sync")
}

pub fn crate_dir(root: &Path, name: &str, version: &str) -> PathBuf {
    let directory = root.join("tooling/native").join(name);
    fs::create_dir_all(&directory).unwrap();
    let contents = format!(
        "[project]\nname = \"{name}\"\nversion = \"{version}\"\n\n[tool.maturin]\nmodule-name = \"{}\"\n",
        name.replace('-', "_")
    );
    fs::write(directory.join("pyproject.toml"), contents).unwrap();
    directory
}

pub fn interpreter(root: &Path, name: &str) -> PathBuf {
    let destination = root.join(name);
    symlink(CHILD, &destination).unwrap();
    destination
}

pub fn declare_server(root: &Path, command: &Path) {
    let config = json!({"mcpServers":{"code-review-graph":{
        "command":command.to_str().unwrap(),
        "args":["-m","conductor.crg_server"],
        "cwd":root.to_str().unwrap()
    }}});
    fs::write(root.join(".mcp.json"), config.to_string()).unwrap();
}

pub fn installed(packages: &Path, distribution: &str, version: &str) {
    let normalized = distribution.replace('-', "_");
    fs::create_dir_all(packages.join(format!("{normalized}-{version}.dist-info"))).unwrap();
}

pub fn packages_of(root: &Path) -> PathBuf {
    root.join("server-venv/site-packages")
}

pub fn tree(case: &mut Case) {
    let root = case.root().to_path_buf();
    fs::create_dir_all(packages_of(&root)).unwrap();
    crate_dir(&root, "conductor-native", VERSION);
    declare_server(&root, &interpreter(&root, "fake-python"));
    case.set_env("FAKE_PURELIB", packages_of(&root).to_str().unwrap());
    case.remove_env("FAKE_IMPORT_ERROR");
}

pub fn consumer_venv(
    root: &Path,
    distribution: &str,
    version: &str,
    origin: Option<Value>,
    extension: bool,
) {
    let normalized = distribution.replace('-', "_");
    let info = root.join(format!(
        ".venv/lib/python3.12/site-packages/{normalized}-{version}.dist-info"
    ));
    fs::create_dir_all(&info).unwrap();
    let payload = if extension {
        format!("{normalized}/{normalized}.cpython-312-x86_64-linux-gnu.so")
    } else {
        format!("{normalized}/__init__.py")
    };
    fs::write(info.join("RECORD"), format!("{payload},,\n")).unwrap();
    if let Some(origin) = origin {
        fs::write(info.join("direct_url.json"), origin.to_string()).unwrap();
    }
}

pub fn git_origin() -> Value {
    json!({"url":"https://github.com/example/forge",
           "vcs_info":{"vcs":"git","commit_id":"c0ffee"},
           "subdirectory":"native/demo-native"})
}

pub fn declared_demo(py: Python<'_>) -> AttrPatch {
    let expected = signature(py, &[], &[]);
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&expected, args, kwargs)?;
            Ok(PyTuple::new(args.py(), ["demo-native"])?
                .into_any()
                .unbind())
        })
        .unwrap();
    AttrPatch::replace(sync(py).as_any(), "declared_names", callback.as_any())
}

pub fn consumer(case: &mut Case) -> AttrPatch {
    let root = case.root().to_path_buf();
    fs::create_dir_all(packages_of(&root)).unwrap();
    consumer_venv(&root, "demo-native", VERSION, Some(git_origin()), true);
    declare_server(&root, &interpreter(&root, "fake-python"));
    case.set_env("FAKE_PURELIB", packages_of(&root).to_str().unwrap());
    case.remove_env("FAKE_IMPORT_ERROR");
    Python::attach(declared_demo)
}

pub fn set_import_error(case: &mut Case) {
    case.set_env("FAKE_IMPORT_ERROR", IMPORT_ERROR);
}

pub fn clear_import_error(py: Python<'_>) {
    module(py, "os")
        .getattr("environ")
        .unwrap()
        .call_method1("pop", ("FAKE_IMPORT_ERROR", py.None()))
        .unwrap();
}

pub fn verdict(py: Python<'_>, root: &Path, check_only: bool) -> (String, Vec<String>) {
    let result = sync(py)
        .getattr("run")
        .unwrap()
        .call1((path(py, root), check_only))
        .unwrap();
    let tuple = result.cast::<PyTuple>().unwrap();
    let detail = tuple.get_item(1).unwrap();
    assert!(detail.cast::<PyList>().is_ok());
    (
        tuple.get_item(0).unwrap().extract().unwrap(),
        detail.extract().unwrap(),
    )
}

pub fn probe_error(py: Python<'_>, error: PyErr) {
    let class = module(py, "conductor.crg_mcp_probe")
        .getattr("ProbeError")
        .unwrap();
    assert_error(py, error, &class, "");
}
