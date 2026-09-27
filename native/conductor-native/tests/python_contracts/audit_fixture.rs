//! Temporary Git and PyO3 fixtures for the CI-history and audit contracts.

use crate::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

const GIT_SELECTORS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG",
    "GIT_TEMPLATE_DIR",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "GIT_PROXY_COMMAND",
];

pub fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in GIT_SELECTORS {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case.set_env("GIT_TERMINAL_PROMPT", "0");
    let root = case.root().to_str().unwrap().to_owned();
    case.set_env("HOME", &root);
    case.set_env("XDG_CONFIG_HOME", &root);
    case.set_env("CUDA_VISIBLE_DEVICES", "");
    case.remove_env("CONDUCTOR_VULTURE_WHITELIST");
    let pylint = case.write("pylint-default.rc", "[MASTER]\n");
    case.set_env("PYLINTRC", pylint.to_str().unwrap());
    case.set_env("PYLINT_HOME", &root);
    case
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for name in GIT_SELECTORS {
        command.env_remove(name);
    }
    let result = command
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run temporary Git fixture command");
    assert!(
        result.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).expect("UTF-8 Git fixture output")
}

pub fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).expect("create fixture parent");
    fs::write(path, contents).expect("write fixture");
}

pub fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn json_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let serialized: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&serialized).unwrap()
}

pub fn capture<'py>(py: Python<'py>, name: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(module(py, "sys").as_any(), name, buffer.as_any());
    (buffer, patch)
}

pub fn captured(buffer: &Bound<'_, PyAny>) -> String {
    buffer.call_method0("getvalue").unwrap().extract().unwrap()
}

pub fn patch_static(target: &Bound<'_, PyAny>, name: &str, value: &Bound<'_, PyAny>) -> AttrPatch {
    let wrapper = module(target.py(), "builtins")
        .getattr("staticmethod")
        .unwrap()
        .call1((value,))
        .unwrap();
    AttrPatch::replace(target, name, wrapper.as_any())
}

pub struct DictItemPatch {
    dict: Py<PyDict>,
    key: String,
    prior: Option<Py<PyAny>>,
}

impl DictItemPatch {
    pub fn replace(dict: &Bound<'_, PyDict>, key: &str, value: &Bound<'_, PyAny>) -> Self {
        let prior = dict.get_item(key).unwrap().map(Bound::unbind);
        dict.set_item(key, value).unwrap();
        Self {
            dict: dict.clone().unbind(),
            key: key.to_owned(),
            prior,
        }
    }
}

impl Drop for DictItemPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            let dict = self.dict.bind(py);
            if let Some(prior) = &self.prior {
                dict.set_item(self.key.as_str(), prior.bind(py)).unwrap();
            } else {
                dict.del_item(self.key.as_str()).unwrap();
            }
        });
    }
}
