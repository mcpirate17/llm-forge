//! Rust fixture construction for the CRG workspace tool contracts.

use crate::comm_support::{bind_signature, py_json, signature};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const SOURCE: &str = concat!(
    "\"\"\"Module.\"\"\"\n\n\ndef compact_state(s):\n",
    "    \"\"\"Compact the state.\"\"\"\n\n\n",
    "class Preamble:\n    def render(self):\n",
    "        \"\"\"Render text.\"\"\"\n\n\n",
    "def compact_other():\n    pass\n"
);

pub fn tools(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.crg_workspace_tools")
}

pub fn summary<'py>(py: Python<'py>, overrides: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyAny> {
    let graph = module(py, "conductor.graph_context");
    let relationship = graph.getattr("GraphRelationship").unwrap();
    let callers = PyList::new(
        py,
        [
            relationship
                .call1(("pkg/mod.py", "CONTAINS", "pkg/mod.py"))
                .unwrap(),
            relationship
                .call1(("pkg/other.py::g", "CALLS", "pkg/other.py"))
                .unwrap(),
        ],
    )
    .unwrap();
    let callees = PyList::new(
        py,
        [
            relationship
                .call1(("pkg/mod.py::f", "CONTAINS", "pkg/mod.py"))
                .unwrap(),
            relationship
                .call1(("pkg/dep.py::h", "CALLS", "pkg/dep.py"))
                .unwrap(),
        ],
    )
    .unwrap();
    let kwargs = PyDict::new(py);
    for (key, value) in [
        (
            "file_path",
            "pkg/mod.py".into_pyobject(py).unwrap().into_any(),
        ),
        (
            "skeleton",
            "def f(): ...".into_pyobject(py).unwrap().into_any(),
        ),
        ("symbols", PyList::new(py, ["f"]).unwrap().into_any()),
        ("callers", callers.into_any()),
        ("callees", callees.into_any()),
        ("graph_status", "ok".into_pyobject(py).unwrap().into_any()),
    ] {
        kwargs.set_item(key, value).unwrap();
    }
    for (key, value) in overrides {
        kwargs.set_item(key, value).unwrap();
    }
    graph
        .getattr("FileContextSummary")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub struct GraphDb {
    pub db: PathBuf,
    _root: AttrPatch,
    _database: AttrPatch,
}

pub fn graph_db(py: Python<'_>, case: &Case) -> GraphDb {
    let src = case.write("pkg/mod.py", SOURCE);
    let db = case.mkdir(".code-review-graph").join("graph.db");
    let sqlite = module(py, "sqlite3");
    let conn = sqlite
        .getattr("connect")
        .unwrap()
        .call1((path(py, &db),))
        .unwrap();
    conn.call_method1("execute", ("CREATE TABLE nodes (qualified_name TEXT, kind TEXT, name TEXT, parent_name TEXT, file_path TEXT, line_start INTEGER, line_end INTEGER)",)).unwrap();
    let source = src.to_str().unwrap();
    let rows: Vec<Value> = vec![
        json!([source, "File", source, null, source, 1, 14]),
        json!([
            format!("{source}::compact_state"),
            "Function",
            "compact_state",
            null,
            source,
            4,
            5
        ]),
        json!([
            format!("{source}::Preamble"),
            "Class",
            "Preamble",
            null,
            source,
            8,
            10
        ]),
        json!([
            format!("{source}::Preamble.render"),
            "Function",
            "render",
            "Preamble",
            source,
            9,
            10
        ]),
        json!([
            format!("{source}::compact_other"),
            "Function",
            "compact_other",
            null,
            source,
            13,
            14
        ]),
    ];
    conn.call_method1(
        "executemany",
        (
            "INSERT INTO nodes VALUES (?,?,?,?,?,?,?)",
            py_json(py, json!(rows)),
        ),
    )
    .unwrap();
    conn.call_method0("commit").unwrap();
    conn.call_method0("close").unwrap();
    let tools = tools(py);
    let root = AttrPatch::replace(tools.as_any(), "ROOT", path(py, case.root()).as_any());
    let expected = signature(py, &["_repo"], &[]);
    let target = db.clone();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&expected, args, kwargs)?;
            Ok(path(args.py(), &target).unbind())
        })
        .unwrap();
    let database = AttrPatch::replace(tools.as_any(), "graph_database_path", callback.as_any());
    GraphDb {
        db,
        _root: root,
        _database: database,
    }
}

fn signature_with_kwargs(py: Python<'_>, name: &str) -> Py<PyAny> {
    let inspect = module(py, "inspect");
    let parameter = inspect.getattr("Parameter").unwrap();
    let ordinary = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
    let rest = parameter.getattr("VAR_KEYWORD").unwrap();
    let params = PyList::empty(py);
    params
        .append(parameter.call1((name, ordinary)).unwrap())
        .unwrap();
    params
        .append(parameter.call1(("kwargs", rest)).unwrap())
        .unwrap();
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((params,))
        .unwrap()
        .unbind()
}

pub struct Recall {
    pub calls: Py<PyList>,
    pub embedders: Py<PyList>,
    pub searches: Py<PyList>,
    _patches: Vec<AttrPatch>,
}

fn embed<'py>(py: Python<'py>, calls: &Bound<'py, PyList>) -> Bound<'py, PyCFunction> {
    let expected = signature_with_kwargs(py, "text");
    let calls = calls.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let bound = bind_signature(&expected, args, kwargs)?;
        let values = bound.getattr("arguments")?;
        let text = values.get_item("text")?;
        let options = values.get_item("kwargs")?;
        let purpose = options.call_method1("get", ("purpose",))?;
        calls
            .bind(args.py())
            .append(PyTuple::new(args.py(), [&text, &purpose])?)?;
        Ok(py_json(args.py(), json!([1.0, 0.0])).unbind())
    })
    .unwrap()
}

fn query<'py>(
    py: Python<'py>,
    embedders: &Bound<'py, PyList>,
    card: &Bound<'py, PyAny>,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["query", "index"], &["top_k", "embedder"]);
    let embedders = embedders.clone().unbind();
    let card = card.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let bound = bind_signature(&expected, args, kwargs)?;
        let values = bound.getattr("arguments")?;
        assert!(values
            .get_item("index")?
            .eq(py_json(args.py(), json!({"kb":true})))?);
        assert!(values.get_item("top_k")?.eq(2)?);
        embedders
            .bind(args.py())
            .append(values.get_item("embedder")?)?;
        Ok(PyList::new(args.py(), [card.bind(args.py())])?
            .into_any()
            .unbind())
    })
    .unwrap()
}

fn search<'py>(
    py: Python<'py>,
    searches: &Bound<'py, PyList>,
    root: &str,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["vector", "rows", "matrix"], &["top_k"]);
    let searches = searches.clone().unbind();
    let path = format!("{root}/research/notes/n.md");
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let bound = bind_signature(&expected, args, kwargs)?;
        let values = bound.getattr("arguments")?;
        searches.bind(args.py()).append(PyTuple::new(
            args.py(),
            [
                values.get_item("vector")?,
                values.get_item("rows")?,
                values.get_item("matrix")?,
                values.get_item("top_k")?,
            ],
        )?)?;
        Ok(py_json(
            args.py(),
            json!([{
                "score":0.51234,"weighted_score":0.6,"source":"notes",
                "path":path,"title":"N","text":"note  text"
            }]),
        )
        .unbind())
    })
    .unwrap()
}

pub fn recall(py: Python<'_>) -> Recall {
    let tools = tools(py);
    let kb = tools.getattr("kb_retrieve").unwrap();
    let memory = tools.getattr("memory_vectors").unwrap();
    let root: String = tools
        .getattr("ROOT")
        .unwrap()
        .str()
        .unwrap()
        .extract()
        .unwrap();
    let card = kb
        .getattr("ScoredCard")
        .unwrap()
        .call1((
            "kb_x.md",
            format!("{root}/research/notes/kb_x.md"),
            0.4567,
            "---\nid: K\n---\n# Card body",
        ))
        .unwrap();
    let calls = PyList::empty(py);
    let embedders = PyList::empty(py);
    let searches = PyList::empty(py);
    let no_args = signature(py, &[], &[]);
    let index =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&no_args, args, kwargs)?;
            Ok(py_json(args.py(), json!({"kb":true})).unbind())
        })
        .unwrap();
    let no_args = signature(py, &[], &[]);
    let sidecar =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&no_args, args, kwargs)?;
            let rows = PyList::new(args.py(), ["row"])?;
            Ok(PyTuple::new(
                args.py(),
                [rows.as_any(), "M".into_pyobject(args.py())?.as_any()],
            )?
            .into_any()
            .unbind())
        })
        .unwrap();
    let patches = vec![
        AttrPatch::replace(&kb, "embed_text", embed(py, &calls).as_any()),
        AttrPatch::replace(&kb, "load_index", index.as_any()),
        AttrPatch::replace(&kb, "query_index", query(py, &embedders, &card).as_any()),
        AttrPatch::replace(&memory, "load_sidecar", sidecar.as_any()),
        AttrPatch::replace(&memory, "search", search(py, &searches, &root).as_any()),
    ];
    Recall {
        calls: calls.unbind(),
        embedders: embedders.unbind(),
        searches: searches.unbind(),
        _patches: patches,
    }
}
