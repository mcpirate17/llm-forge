#![cfg(feature = "python-compat-tests")]
//! Rust assertions for the CRG path and docstring embedding text contract.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{module, path, text, Case};

const SOURCE: &str = r#""""Module doc."""


def plain(a: int) -> int:
    """First line of plain.

    Second paragraph.
    """
    return a


@decorator
def decorated():
    """Decorated doc."""


class Widget:
    """Widget class doc."""

    def method(self, x):
        """Method doc."""

    def nodoc(self):
        pass


def plain_twin():
    """Twin."""
"#;

fn fixture(py: Python<'_>, case: &Case, cet: &Bound<'_, PyModule>) -> std::path::PathBuf {
    let source = case.write("pkg/mod.py", SOURCE);
    cet.getattr("_python_docstrings")
        .unwrap()
        .call_method0("cache_clear")
        .unwrap();
    let _ = py;
    source
}

fn node<'py>(
    py: Python<'py>,
    file: &Path,
    name: &str,
    line: i64,
    extra: &[(&str, Bound<'py, PyAny>)],
) -> Bound<'py, PyAny> {
    let fields = PyDict::new(py);
    fields.set_item("kind", "Function").unwrap();
    fields.set_item("name", name).unwrap();
    fields
        .set_item("qualified_name", format!("{}::{name}", file.display()))
        .unwrap();
    fields
        .set_item("file_path", file.to_str().unwrap())
        .unwrap();
    fields.set_item("line_start", line).unwrap();
    for key in ["parent_name", "params", "return_type"] {
        fields.set_item(key, py.None()).unwrap();
    }
    for (key, value) in extra {
        fields.set_item(key, value).unwrap();
    }
    PyModule::import(py, "types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&fields))
        .unwrap()
}

fn doc(cet: &Bound<'_, PyModule>, node: &Bound<'_, PyAny>) -> String {
    text(
        &cet.getattr("docstring_for")
            .unwrap()
            .call1((node,))
            .unwrap(),
    )
}

#[test]
fn docstrings_for_functions_methods_classes_and_decorators() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let file = fixture(py, &case, &cet);
        assert_eq!(
            doc(&cet, &node(py, &file, "plain", 4, &[])),
            "First line of plain."
        );
        assert_eq!(
            doc(&cet, &node(py, &file, "decorated", 13, &[])),
            "Decorated doc."
        );
        let kind = "Class".into_pyobject(py).unwrap().into_any();
        assert_eq!(
            doc(&cet, &node(py, &file, "Widget", 18, &[("kind", kind)])),
            "Widget class doc."
        );
        let parent = "Widget".into_pyobject(py).unwrap().into_any();
        let qualified = format!("{}::Widget.method", file.display())
            .into_pyobject(py)
            .unwrap()
            .into_any();
        assert_eq!(
            doc(
                &cet,
                &node(
                    py,
                    &file,
                    "method",
                    21,
                    &[("parent_name", parent), ("qualified_name", qualified)]
                )
            ),
            "Method doc."
        );
        let parent = "Widget".into_pyobject(py).unwrap().into_any();
        assert_eq!(
            doc(
                &cet,
                &node(py, &file, "nodoc", 24, &[("parent_name", parent)])
            ),
            ""
        );
    });
}

#[test]
fn docstring_prefers_line_proximity_then_unique_name() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let file = fixture(py, &case, &cet);
        assert_eq!(doc(&cet, &node(py, &file, "plain_twin", 999, &[])), "Twin.");
        assert_eq!(doc(&cet, &node(py, &file, "missing", 1, &[])), "");
    });
}

#[test]
fn non_python_and_unreadable_files_yield_no_docstring() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let rust = case.write("lib.rs", "fn main() {}");
        assert_eq!(doc(&cet, &node(py, &rust, "main", 1, &[])), "");
        assert_eq!(
            doc(&cet, &node(py, &case.root().join("gone.py"), "x", 1, &[])),
            ""
        );
        let broken = case.write("broken.py", "def (:\n");
        assert_eq!(doc(&cet, &node(py, &broken, "x", 1, &[])), "");
    });
}

#[test]
fn node_text_is_relative_and_bounded() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let file = fixture(py, &case, &cet);
        let params = "(a: int)".into_pyobject(py).unwrap().into_any();
        let returns = "int".into_pyobject(py).unwrap().into_any();
        let node = node(
            py,
            &file,
            "plain",
            4,
            &[("params", params), ("return_type", returns)],
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        let render = || {
            text(
                &cet.getattr("node_text")
                    .unwrap()
                    .call((&node,), Some(&kwargs))
                    .unwrap(),
            )
        };
        assert_eq!(
            render(),
            "pkg/mod.py::plain function (a: int) returns int — First line of plain."
        );
        node.setattr("params", "(\n    a: int,\n    *,\n    b: str,\n)")
            .unwrap();
        assert!(render().contains("( a: int, *, b: str, )"));
        node.setattr("params", format!("({})", "x, ".repeat(400)))
            .unwrap();
        let rendered = render();
        let limit: usize = cet.getattr("TEXT_CHARS").unwrap().extract().unwrap();
        assert_eq!(rendered.chars().count(), limit);
        assert!(rendered.ends_with('…'));
    });
}

#[test]
fn docstring_cache_is_keyed_by_mtime_and_size() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let file = fixture(py, &case, &cet);
        let node = node(py, &file, "plain", 4, &[]);
        assert_eq!(doc(&cet, &node), "First line of plain.");
        fs::write(&file, SOURCE.replace("First line of plain.", "Changed.")).unwrap();
        assert_eq!(doc(&cet, &node), "Changed.");
        let cache = cet
            .getattr("_python_docstrings")
            .unwrap()
            .call_method0("cache_info")
            .unwrap();
        assert_eq!(
            cache
                .getattr("maxsize")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            64
        );
    });
}

struct ModulesRestore(Vec<(String, Option<Py<PyAny>>)>);

impl ModulesRestore {
    fn insert(&mut self, py: Python<'_>, name: &str, value: &Bound<'_, PyAny>) {
        let modules = PyModule::import(py, "sys")
            .unwrap()
            .getattr("modules")
            .unwrap();
        let prior = modules.get_item(name).ok().map(Bound::unbind);
        modules.set_item(name, value).unwrap();
        self.0.push((name.to_owned(), prior));
    }
}

impl Drop for ModulesRestore {
    fn drop(&mut self) {
        Python::attach(|py| {
            let modules = PyModule::import(py, "sys")
                .unwrap()
                .getattr("modules")
                .unwrap();
            for (name, prior) in self.0.drain(..).rev() {
                match prior {
                    Some(value) => modules.set_item(name, value.bind(py)).unwrap(),
                    None => {
                        modules.del_item(name).unwrap();
                    }
                }
            }
        });
    }
}

#[test]
fn install_replaces_pinned_node_to_text() {
    let case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let file = fixture(py, &case, &cet);
        let mut restored = ModulesRestore(Vec::new());
        let fake_pkg = PyModule::new(py, "code_review_graph").unwrap();
        let fake_embeddings = PyModule::new(py, "code_review_graph.embeddings").unwrap();
        let stock = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _| -> PyResult<&str> { Ok("stock") },
        )
        .unwrap();
        fake_embeddings.setattr("_node_to_text", stock).unwrap();
        fake_pkg.setattr("embeddings", &fake_embeddings).unwrap();
        restored.insert(py, "code_review_graph", fake_pkg.as_any());
        restored.insert(py, "code_review_graph.embeddings", fake_embeddings.as_any());
        let checked = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls = Arc::clone(&checked);
        let fake_bridge = PyModule::new(py, "conductor.crg_embedding_bridge").unwrap();
        let check = PyCFunction::new_closure(
            py,
            None,
            None,
            move |_args: &Bound<'_, PyTuple>, _| -> PyResult<()> {
                calls.lock().unwrap().push("checked".to_owned());
                Ok(())
            },
        )
        .unwrap();
        fake_bridge.setattr("assert_supported_crg", check).unwrap();
        restored.insert(py, "conductor.crg_embedding_bridge", fake_bridge.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        cet.getattr("install_node_text")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        assert_eq!(*checked.lock().unwrap(), vec!["checked"]);
        let result = fake_embeddings
            .getattr("_node_to_text")
            .unwrap()
            .call1((node(py, &file, "plain", 4, &[]),))
            .unwrap();
        assert!(text(&result).starts_with("pkg/mod.py::plain function"));
    });
}

#[test]
fn first_line_boundary_returns_empty_for_whitespace_only_docstring() {
    let _case = Case::new();
    Python::attach(|py| {
        let cet = module(py, "conductor.crg_embedding_text");
        let first = cet.getattr("_first_line").unwrap();
        for value in [None, Some(""), Some(" \n \n ")] {
            assert_eq!(text(&first.call1((value,)).unwrap()), "");
        }
    });
}
