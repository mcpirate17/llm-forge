//! Rust-owned fixtures for candidate scope contracts.

use crate::git_fixture_support::{git, write};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs;
use std::path::Path;

pub const CRATE: &str = "research/runtime/native/rust/probe";
pub const SOURCE: &str = "research/runtime/native/rust/probe/src/lib.rs";
pub const PROBE_PATH: &str = "research/tests/test_probe.py";
pub const LEGACY: [&str; 2] = ["test_probe_legacy", "test_probe_older"];

pub const TESTED: &str = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn adds() {\n        assert_eq!(add(1, 2), 3);\n    }\n}\n";
pub const UNTESTED: &str = "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n";

/// Match the source bytes in the historical Python fixture, including its final newline.
pub fn probe_source(labels: &[&str]) -> String {
    let mut lines = Vec::new();
    for label in labels {
        if let Some((owner, method)) = label.split_once("::") {
            lines.push(format!("class {owner}:"));
            lines.push(format!("    def {method}(self) -> None:"));
            lines.push("        assert True".to_owned());
        } else {
            lines.push(format!("def {label}() -> None:"));
            lines.push("    assert True".to_owned());
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// A real Git anchor and byte-pinned inventory, with patches restored on drop.
pub struct GateFixture {
    patches: RefCell<Vec<AttrPatch>>,
    context: Py<PyAny>,
}

fn anchor_inventory(
    py: Python<'_>,
    root: &Path,
    snapshot: &Path,
    inventory: &Bound<'_, PyDict>,
) -> Vec<AttrPatch> {
    write(snapshot, "conductor/mutation_campaigns/registry.json", "{}");
    fs::create_dir_all(root).unwrap();
    git(root, &["init", "--quiet", "--initial-branch=main"]);
    git(root, &["config", "user.name", "Candidate Review Test"]);
    git(
        root,
        &["config", "user.email", "candidate-review@example.invalid"],
    );

    let mut paths = Vec::new();
    for (key, labels) in inventory.iter() {
        let relative: String = key.extract().unwrap();
        let labels: Vec<String> = labels.extract().unwrap();
        let names: Vec<&str> = labels.iter().map(String::as_str).collect();
        write(root, &relative, &probe_source(&names));
        paths.push(relative);
    }
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    git(root, &add);
    git(
        root,
        &["commit", "--quiet", "--allow-empty", "--message", "anchor"],
    );

    let verification = module(py, "conductor.candidate_review.verification");
    let anchor_commit = git(root, &["rev-parse", "HEAD"]);
    let anchor_tree = git(root, &["rev-parse", "HEAD^{tree}"]);
    let mut patches = Vec::new();
    patches.push(AttrPatch::replace(
        verification.as_any(),
        "GRANDFATHER_ANCHOR_COMMIT_OID",
        anchor_commit.into_pyobject(py).unwrap().as_any(),
    ));
    patches.push(AttrPatch::replace(
        verification.as_any(),
        "GRANDFATHER_ANCHOR_TREE_OID",
        anchor_tree.into_pyobject(py).unwrap().as_any(),
    ));

    let policy_module = module(py, "conductor.candidate_review.policy");
    let payload = PyDict::new(py);
    let version: u32 = verification
        .getattr("GRANDFATHER_SCHEMA_VERSION")
        .unwrap()
        .extract()
        .unwrap();
    payload
        .set_item(
            "schema",
            format!("conductor.candidate_review.grandfather_inventory/v{version}"),
        )
        .unwrap();
    payload
        .set_item(
            "anchor_commit",
            policy_module
                .getattr("MUTATION_WAIVER_SOURCE_ANCHOR")
                .unwrap(),
        )
        .unwrap();
    payload
        .set_item(
            "milestone",
            policy_module
                .getattr("W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE")
                .unwrap(),
        )
        .unwrap();
    payload.set_item("tests", inventory).unwrap();
    let inventory_text: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((payload,))
        .unwrap()
        .extract()
        .unwrap();
    let inventory_relpath: String = verification
        .getattr("GRANDFATHER_INVENTORY_RELPATH")
        .unwrap()
        .extract()
        .unwrap();
    write(snapshot, &inventory_relpath, &inventory_text);
    let digest = format!("{:x}", Sha256::digest(inventory_text.as_bytes()));
    patches.push(AttrPatch::replace(
        verification.as_any(),
        "GRANDFATHER_INVENTORY_SHA256",
        digest.into_pyobject(py).unwrap().as_any(),
    ));
    patches
}

fn review_context<'py>(py: Python<'py>, root: &Path, snapshot: &Path) -> Bound<'py, PyAny> {
    write(
        snapshot,
        PROBE_PATH,
        "def test_probe_new():\n    assert True\n",
    );
    let change = test_change(py, PROBE_PATH);
    let model = module(py, "conductor.candidate_review.model");
    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs.set_item("kind", "index").unwrap();
    candidate_kwargs
        .set_item("tree_oid", "a".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_tree_oid", "b".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_commit_oid", "c".repeat(40))
        .unwrap();
    candidate_kwargs.set_item("commit_oid", py.None()).unwrap();
    candidate_kwargs.set_item("target_ref", "HEAD").unwrap();
    candidate_kwargs
        .set_item("changes", PyTuple::new(py, [change]).unwrap())
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kwargs))
        .unwrap();
    let policy_path = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    let policy = module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((policy_path,))
        .unwrap();
    let context_kwargs = PyDict::new(py);
    context_kwargs.set_item("repo", path(py, root)).unwrap();
    context_kwargs
        .set_item("snapshot", path(py, snapshot))
        .unwrap();
    context_kwargs.set_item("candidate", candidate).unwrap();
    context_kwargs
        .set_item("entries", PyTuple::empty(py))
        .unwrap();
    context_kwargs.set_item("policy", policy).unwrap();
    context_kwargs.set_item("surface", "manual").unwrap();
    context_kwargs.set_item("profile", "fast").unwrap();
    context_kwargs.set_item("owner", py.None()).unwrap();
    context_kwargs
        .set_item("runtime_dir", path(py, &root.join("runtime")))
        .unwrap();
    let context = module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&context_kwargs))
        .unwrap();
    context
}

impl GateFixture {
    pub fn new(py: Python<'_>, case: &Case, inventory: &Bound<'_, PyDict>) -> Self {
        let root = case.root();
        let snapshot = root.join("snapshot");
        let patches = anchor_inventory(py, root, &snapshot, inventory);
        let context = review_context(py, root, &snapshot);
        Self {
            patches: RefCell::new(patches),
            context: context.unbind(),
        }
    }

    pub fn context<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.context.bind(py).clone()
    }

    pub fn setattr(
        &self,
        _py: Python<'_>,
        target: &Bound<'_, PyAny>,
        name: &str,
        value: &Bound<'_, PyAny>,
    ) {
        self.patches
            .borrow_mut()
            .push(AttrPatch::replace(target, name, value));
    }
}

impl Drop for GateFixture {
    fn drop(&mut self) {
        // Repeated patches to one attribute must restore in the same LIFO order
        // as pytest.MonkeyPatch.undo in the original fixture.
        while self.patches.get_mut().pop().is_some() {}
    }
}

pub fn write_snapshot(ctx: &Bound<'_, PyAny>, relative: &str, contents: &str) {
    let root: String = ctx
        .getattr("snapshot")
        .unwrap()
        .str()
        .unwrap()
        .extract()
        .unwrap();
    write(Path::new(&root), relative, contents);
}

pub fn changed_context<'py>(
    py: Python<'py>,
    context: &Bound<'py, PyAny>,
    changes: &[Bound<'py, PyAny>],
) -> Bound<'py, PyAny> {
    let replace = module(py, "dataclasses").getattr("replace").unwrap();
    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs
        .set_item("changes", PyTuple::new(py, changes).unwrap())
        .unwrap();
    let candidate = replace
        .call(
            (context.getattr("candidate").unwrap(),),
            Some(&candidate_kwargs),
        )
        .unwrap();
    let context_kwargs = PyDict::new(py);
    context_kwargs.set_item("candidate", candidate).unwrap();
    replace.call((context,), Some(&context_kwargs)).unwrap()
}

/// Match the added Python test change from the original hardening fixture.
pub fn test_change<'py>(py: Python<'py>, source: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.model")
        .getattr("Change")
        .unwrap()
        .call1((
            "A",
            source,
            py.None(),
            "000000",
            "100644",
            "0".repeat(40),
            "1".repeat(40),
            PyTuple::new(py, ["python", "source", "test"]).unwrap(),
        ))
        .unwrap()
}

pub fn native_change<'py>(py: Python<'py>, source: &str, risk: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.model")
        .getattr("Change")
        .unwrap()
        .call1((
            "M",
            source,
            py.None(),
            "100644",
            "100644",
            "1".repeat(40),
            "2".repeat(40),
            PyTuple::new(py, ["native", "source"]).unwrap(),
            risk,
        ))
        .unwrap()
}

pub fn python_change<'py>(py: Python<'py>, source: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.model")
        .getattr("Change")
        .unwrap()
        .call1((
            "M",
            source,
            py.None(),
            "100644",
            "100644",
            "1".repeat(40),
            "2".repeat(40),
            PyTuple::new(py, ["python", "source"]).unwrap(),
            "normal",
        ))
        .unwrap()
}

pub fn signature_with_kwargs(py: Python<'_>, positional: &[&str], kwargs: &str) -> Py<PyAny> {
    let inspect = py.import("inspect").unwrap();
    let parameter = inspect.getattr("Parameter").unwrap();
    let parts = PyList::empty(py);
    for name in positional {
        parts
            .append(
                parameter
                    .call1((*name, parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap()))
                    .unwrap(),
            )
            .unwrap();
    }
    parts
        .append(
            parameter
                .call1((kwargs, parameter.getattr("VAR_KEYWORD").unwrap()))
                .unwrap(),
        )
        .unwrap();
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((parts,))
        .unwrap()
        .unbind()
}
