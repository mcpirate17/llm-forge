//! Rust fixtures and capture guards for session-preamble API contracts.

use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use std::path::{Path, PathBuf};

pub const FIXTURE_PREAMBLE: &[&str] = &[
    "MISSION: Ship correct, minimal, native-speed governance tooling.",
    "RETRIEVE (do not dump .current_work.md): `python -m conductor.kb_retrieve query \"<task>\" --top-k 5`.",
    "FLEET: embed http://127.0.0.1:7317/v1 (GPU-guest). Clerk models are clerical-only, zero approval authority; never gate work or runs on local output.",
    "MUTATION: mutate ONLY the files you changed and their tests -- never repo-wide. automatic engines only, hand-authored mutants forbidden.",
    "DELEGATE: searches touching >3 files go to a subagent. Prefer ast_context_tool/query_graph over whole-file Read.",
];

pub fn policy(root: &Path, preamble: &[&str], mandates: &[&str]) {
    fs::create_dir_all(root).expect("create policy repository");
    let lines = serde_json::to_string(preamble).unwrap();
    let mandates = serde_json::to_string(mandates).unwrap();
    fs::write(
        root.join("pyproject.toml"),
        format!("[tool.conductor.session]\npreamble = {lines}\nstanding_mandates = {mandates}\n"),
    )
    .expect("write fixture policy");
}

pub fn fixture_repo(case: &Case) -> PathBuf {
    let root = case.root().join("fixture-project");
    policy(&root, FIXTURE_PREAMBLE, &[]);
    root
}

pub fn state<'py>(py: Python<'py>) -> Bound<'py, PyDict> {
    let claim = PyDict::new(py);
    claim.set_item("claim_id", "claim-x").unwrap();
    claim.set_item("owner", "grok").unwrap();
    claim
        .set_item("paths", vec!["conductor/session_preamble.py"; 40])
        .unwrap();
    claim
        .set_item("justification", "should not appear in inject")
        .unwrap();
    let state = PyDict::new(py);
    state
        .set_item(
            "standing_mandates",
            [
                "NOVEL_MECHANISMS_ONLY: never softmax twins.",
                "MEMORY_RETRIEVE: query, do not dump.",
            ],
        )
        .unwrap();
    state
        .set_item("active_headings", ["heading-a", "heading-b"])
        .unwrap();
    state.set_item("active_claims", [claim]).unwrap();
    state
}

pub fn render<'py>(
    py: Python<'py>,
    preamble: &Bound<'py, PyModule>,
    state: &Bound<'py, PyAny>,
    repo: Option<&Path>,
    name: &str,
    summary: &str,
    max_chars: Option<usize>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("state", state).unwrap();
    kwargs.set_item("a2a_name", name).unwrap();
    kwargs.set_item("a2a_summary", summary).unwrap();
    if let Some(repo) = repo {
        kwargs.set_item("repo", path(py, repo)).unwrap();
    }
    if let Some(max_chars) = max_chars {
        kwargs.set_item("max_chars", max_chars).unwrap();
    }
    preamble
        .getattr("render_text")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn capture<T>(py: Python<'_>, action: impl FnOnce() -> T) -> (T, String, String) {
    let io = module(py, "io");
    let stdout = io.call_method0("StringIO").unwrap();
    let stderr = io.call_method0("StringIO").unwrap();
    let sys = module(py, "sys");
    let _out = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
    let _err = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
    let value = action();
    let out = stdout.call_method0("getvalue").unwrap().extract().unwrap();
    let err = stderr.call_method0("getvalue").unwrap().extract().unwrap();
    (value, out, err)
}

pub struct ModulePatch {
    modules: Py<PyDict>,
    name: String,
    original: Option<Py<PyAny>>,
}

impl ModulePatch {
    pub fn new(py: Python<'_>, name: &str, replacement: &Bound<'_, PyModule>) -> Self {
        let modules = module(py, "sys")
            .getattr("modules")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let original = modules.get_item(name).unwrap().map(Bound::unbind);
        modules.set_item(name, replacement).unwrap();
        Self {
            modules: modules.unbind(),
            name: name.to_owned(),
            original,
        }
    }
}

impl Drop for ModulePatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            let modules = self.modules.bind(py);
            if let Some(original) = &self.original {
                modules.set_item(&self.name, original.bind(py)).unwrap();
            } else {
                modules.del_item(&self.name).unwrap();
            }
        });
    }
}
