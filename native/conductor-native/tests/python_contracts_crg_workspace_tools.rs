#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for CRG workspace MCP tools.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/crg_workspace_support.rs"]
mod workspace_support;

use comm_support::{bind_signature, py_json, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::path::Path;
use support::{assert_error, module, path, text, AttrPatch, Case};
use workspace_support::{graph_db, recall, summary, tools};

fn equal_json(actual: &Bound<'_, PyAny>, expected: Value) {
    let other = py_json(actual.py(), expected);
    assert!(
        actual.eq(&other).unwrap(),
        "actual: {}",
        actual.repr().unwrap()
    );
}

fn value_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    let value_error = module(py, "builtins").getattr("ValueError").unwrap();
    assert_error(py, result.unwrap_err(), &value_error, part);
}

#[test]
fn snippet_strips_frontmatter_collapses_whitespace_and_truncates() {
    let _case = Case::new();
    Python::attach(|py| {
        let snippet = tools(py).getattr("_snippet").unwrap();
        assert!(snippet
            .call1(("---\nid: X\n---\n\n# Title\n\nline  one\nline two\n",))
            .unwrap()
            .eq("# Title line one line two")
            .unwrap());
        let kwargs = PyDict::new(py);
        kwargs.set_item("limit", 20).unwrap();
        let output: String = snippet
            .call(("word ".repeat(100),), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(output.chars().count(), 20);
        assert!(output.ends_with('…'));
    });
}

#[test]
fn snippet_without_closing_frontmatter_keeps_text() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(tools(py)
            .getattr("_snippet")
            .unwrap()
            .call1(("--- not really frontmatter",))
            .unwrap()
            .eq("--- not really frontmatter")
            .unwrap());
    });
}

#[test]
fn short_path_only_strips_repo_root() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let root: String = tools
            .getattr("ROOT")
            .unwrap()
            .str()
            .unwrap()
            .extract()
            .unwrap();
        let short = tools.getattr("_short_path").unwrap();
        assert!(short
            .call1((format!("{root}/a/b.md"),))
            .unwrap()
            .eq("a/b.md")
            .unwrap());
        assert!(short
            .call1(("/elsewhere/a.md",))
            .unwrap()
            .eq("/elsewhere/a.md")
            .unwrap());
    });
}

#[test]
fn ast_context_filters_contains_edges() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let seen = PyDict::new(py);
        let expected = signature(
            py,
            &["repo", "file_path", "target_symbol", "with_graph"],
            &[],
        );
        let captured = seen.clone().unbind();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let values = bound.getattr("arguments")?;
                let recorded = captured.bind(args.py());
                for (field, key) in [
                    ("repo", "repo"),
                    ("file_path", "file_path"),
                    ("target_symbol", "symbol"),
                    ("with_graph", "graph"),
                ] {
                    recorded.set_item(key, values.get_item(field)?)?;
                }
                Ok(summary(args.py(), &[]).unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(tools.as_any(), "get_file_context", callback.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("symbol", "f").unwrap();
        kwargs.set_item("repo_root", "/r").unwrap();
        let out: String = tools
            .getattr("ast_context")
            .unwrap()
            .call(("pkg/mod.py",), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        let expected_seen = PyDict::new(py);
        expected_seen
            .set_item("repo", path(py, Path::new("/r")))
            .unwrap();
        expected_seen.set_item("file_path", "pkg/mod.py").unwrap();
        expected_seen.set_item("symbol", "f").unwrap();
        expected_seen.set_item("graph", true).unwrap();
        assert!(seen.eq(expected_seen).unwrap());
        assert!(!out.contains("CONTAINS"));
        for part in [
            "`pkg/other.py::g` (CALLS)",
            "`pkg/dep.py::h` (CALLS)",
            "def f(): ...",
        ] {
            assert!(out.contains(part), "missing {part}: {out}");
        }
    });
}

#[test]
fn ast_context_caps_output_and_says_how_to_narrow() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let max: usize = tools
            .getattr("AST_CONTEXT_MAX_BYTES")
            .unwrap()
            .extract()
            .unwrap();
        let big = summary(
            py,
            &[(
                "skeleton",
                "x".repeat(max + 500).into_pyobject(py).unwrap().into_any(),
            )],
        )
        .unbind();
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, _kwargs| -> PyResult<Py<PyAny>> { Ok(big.clone_ref(args.py())) },
        )
        .unwrap();
        let _patch = AttrPatch::replace(tools.as_any(), "get_file_context", callback.as_any());
        let out: String = tools
            .getattr("ast_context")
            .unwrap()
            .call1(("pkg/mod.py",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(out.len() < max + 120);
        assert!(out.contains("truncated") && out.contains("pass symbol=<name>"));
        let kwargs = PyDict::new(py);
        kwargs.set_item("symbol", "f").unwrap();
        let narrowed: String = tools
            .getattr("ast_context")
            .unwrap()
            .call(("pkg/mod.py",), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert!(narrowed.contains("offset/limit") && !narrowed.contains("pass symbol"));
        let small = summary(py, &[]).unbind();
        let clean = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, _kwargs| -> PyResult<Py<PyAny>> { Ok(small.clone_ref(args.py())) },
        )
        .unwrap();
        let _clean = AttrPatch::replace(tools.as_any(), "get_file_context", clean.as_any());
        let output: String = tools
            .getattr("ast_context")
            .unwrap()
            .call1(("pkg/mod.py",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(!output.contains("truncated"));
    });
}

#[test]
fn ast_context_reports_errors_as_text() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let class = module(py, "conductor.graph_context")
            .getattr("GraphContextError")
            .unwrap()
            .unbind();
        let boom = PyCFunction::new_closure(py, None, None, move |args, _kwargs| -> PyResult<()> {
            Err(PyErr::from_value(
                class
                    .bind(args.py())
                    .call1(("symbol 'nope' not found",))?
                    .as_any()
                    .clone(),
            ))
        })
        .unwrap();
        let _patch = AttrPatch::replace(tools.as_any(), "get_file_context", boom.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("symbol", "nope").unwrap();
        assert!(tools
            .getattr("ast_context")
            .unwrap()
            .call(("pkg/mod.py",), Some(&kwargs))
            .unwrap()
            .eq("ast_context ERROR: symbol 'nope' not found")
            .unwrap());
    });
}

#[test]
fn workspace_recall_rejects_empty_query() {
    let _case = Case::new();
    Python::attach(|py| {
        value_error(
            py,
            tools(py)
                .getattr("workspace_recall")
                .unwrap()
                .call1(("   ",)),
            "empty",
        )
    });
}

#[test]
fn register_workspace_tools_uses_crg_tool_naming() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let records = PyList::empty(py);
        let captured = records.clone().unbind();
        let expected = signature(py, &["name"], &[]);
        let tool =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let name = bound.getattr("arguments")?.get_item("name")?.unbind();
                let decorate_signature = signature(args.py(), &["fn"], &[]);
                let records = captured.clone_ref(args.py());
                let decorate = PyCFunction::new_closure(
                    args.py(),
                    None,
                    None,
                    move |call_args, call_kwargs| -> PyResult<Py<PyAny>> {
                        let bound = bind_signature(&decorate_signature, call_args, call_kwargs)?;
                        let fn_ = bound.getattr("arguments")?.get_item("fn")?;
                        records.bind(call_args.py()).append(PyTuple::new(
                            call_args.py(),
                            [name.bind(call_args.py()), &fn_],
                        )?)?;
                        Ok(fn_.unbind())
                    },
                )?;
                Ok(decorate.into_any().unbind())
            })
            .unwrap();
        let fake = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call0()
            .unwrap();
        fake.setattr("tool", tool).unwrap();
        let names = tools
            .getattr("register_workspace_tools")
            .unwrap()
            .call1((fake,))
            .unwrap();
        let expected = [
            "ast_context_tool",
            "workspace_recall_tool",
            "locate_tool",
            "symbol_source_tool",
            "session_brief_tool",
        ];
        assert!(names.eq(PyList::new(py, expected).unwrap()).unwrap());
        let expected_records = PyDict::new(py);
        for (name, function) in [
            ("ast_context_tool", "ast_context"),
            ("workspace_recall_tool", "workspace_recall"),
            ("locate_tool", "locate"),
            ("symbol_source_tool", "symbol_source"),
            ("session_brief_tool", "session_brief_tool"),
        ] {
            expected_records
                .set_item(name, tools.getattr(function).unwrap())
                .unwrap();
        }
        let actual = module(py, "builtins")
            .getattr("dict")
            .unwrap()
            .call1((records,))
            .unwrap();
        assert!(actual.eq(expected_records).unwrap());
    });
}

#[test]
fn locate_exact_then_prefix_with_docstrings() {
    let case = Case::new();
    Python::attach(|py| {
        let _fixture = graph_db(py, &case);
        let locate = tools(py).getattr("locate").unwrap();
        let output = locate.call1(("compact",)).unwrap();
        assert!(output.get_item("status").unwrap().eq("ok").unwrap());
        let rows = output.get_item("results").unwrap();
        let names = PyList::empty(py);
        for row in rows.try_iter().unwrap() {
            names
                .append(row.unwrap().get_item("qualified_name").unwrap())
                .unwrap();
        }
        assert!(names
            .eq(PyList::new(
                py,
                ["pkg/mod.py::compact_other", "pkg/mod.py::compact_state"]
            )
            .unwrap())
            .unwrap());
        let exact = locate
            .call1(("compact_state",))
            .unwrap()
            .get_item("results")
            .unwrap()
            .get_item(0)
            .unwrap();
        equal_json(
            &exact,
            json!({"qualified_name":"pkg/mod.py::compact_state","kind":"Function","line":4,"doc":"Compact the state."}),
        );
        let other = locate
            .call1(("compact_other",))
            .unwrap()
            .get_item("results")
            .unwrap()
            .get_item(0)
            .unwrap();
        assert!(other.get_item("doc").is_err());
        let method = locate
            .call1(("render",))
            .unwrap()
            .get_item("results")
            .unwrap()
            .get_item(0)
            .unwrap();
        assert!(method
            .get_item("qualified_name")
            .unwrap()
            .eq("pkg/mod.py::Preamble.render")
            .unwrap());
        assert!(method.get_item("doc").unwrap().eq("Render text.").unwrap());
        let kwargs = PyDict::new(py);
        kwargs.set_item("kind", "Class").unwrap();
        assert_eq!(
            locate
                .call(("compact",), Some(&kwargs))
                .unwrap()
                .get_item("results")
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        kwargs.set_item("limit", 1).unwrap();
        assert!(locate
            .call(("Pre",), Some(&kwargs))
            .unwrap()
            .get_item("results")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item("kind")
            .unwrap()
            .eq("Class")
            .unwrap());
        let one = PyDict::new(py);
        one.set_item("limit", 1).unwrap();
        assert_eq!(
            locate
                .call(("compact",), Some(&one))
                .unwrap()
                .get_item("results")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
    });
}

#[test]
fn locate_rejects_empty_name_and_missing_graph() {
    let case = Case::new();
    Python::attach(|py| {
        let fixture = graph_db(py, &case);
        let tools = tools(py);
        value_error(py, tools.getattr("locate").unwrap().call1(("  ",)), "empty");
        let expected = signature(py, &["_repo"], &[]);
        let missing = fixture.db.join("nope");
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                bind_signature(&expected, args, kwargs)?;
                Ok(path(args.py(), &missing).unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(tools.as_any(), "graph_database_path", callback.as_any());
        let error = tools
            .getattr("locate")
            .unwrap()
            .call1(("compact",))
            .unwrap_err();
        assert_error(
            py,
            error,
            &module(py, "builtins").getattr("FileNotFoundError").unwrap(),
            "graph missing",
        );
    });
}

#[test]
fn enrich_search_results_adds_doc_and_drops_noise() {
    let case = Case::new();
    Python::attach(|py| {
        let fixture = graph_db(py, &case);
        let src = fixture
            .db
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("pkg/mod.py");
        let source = src.to_str().unwrap();
        let payload = py_json(
            py,
            json!({"status":"ok","results":[
                {"name":"compact_state","kind":"Function","file_path":source,"score":0.016},
                {"name":"render","kind":"Function","file_path":source,"signature":"def render(self)","params":"(self)","return_type":null,"line_start":9,"line_end":10,"parent_name":"Preamble","score":0.015},
                {"name":"ghost","kind":"Function","file_path":source,"score":0.01},
                "not a dict"
            ]}),
        );
        let enrich = tools(py).getattr("enrich_search_results").unwrap();
        let output = enrich.call1((payload,)).unwrap();
        assert!(output.get_item("status").unwrap().eq("ok").unwrap());
        let rows = output.get_item("results").unwrap();
        assert_eq!(rows.len().unwrap(), 4);
        equal_json(
            &rows.get_item(0).unwrap(),
            json!({"name":"compact_state","kind":"Function","file_path":source,"qualified_name":"pkg/mod.py::compact_state","line":4,"doc":"Compact the state."}),
        );
        let second = rows.get_item(1).unwrap();
        assert!(second
            .get_item("signature")
            .unwrap()
            .eq("def render(self)")
            .unwrap());
        assert!(second.get_item("doc").unwrap().eq("Render text.").unwrap());
        let keys = second.call_method0("keys").unwrap();
        for key in ["score", "params", "return_type", "line_end", "parent_name"] {
            assert!(!keys.contains(key).unwrap());
        }
        equal_json(
            &rows.get_item(2).unwrap(),
            json!({"name":"ghost","kind":"Function","file_path":source}),
        );
        assert!(rows.get_item(3).unwrap().eq("not a dict").unwrap());
        assert!(enrich.call1(("text",)).unwrap().eq("text").unwrap());
        equal_json(
            &enrich
                .call1((py_json(py, json!({"status":"error"})),))
                .unwrap(),
            json!({"status":"error"}),
        );
    });
}

#[test]
fn search_enrichers_target_semantic_search() {
    let case = Case::new();
    Python::attach(|py| {
        let _fixture = graph_db(py, &case);
        let enrichers = tools(py)
            .getattr("search_enrichers")
            .unwrap()
            .call0()
            .unwrap();
        let names = module(py, "builtins")
            .getattr("list")
            .unwrap()
            .call1((&enrichers,))
            .unwrap();
        assert!(names
            .eq(PyList::new(py, ["semantic_search_nodes_tool"]).unwrap())
            .unwrap());
        let input = py_json(py, json!({"results":[]}));
        let output = enrichers
            .get_item("semantic_search_nodes_tool")
            .unwrap()
            .call1((input,))
            .unwrap();
        equal_json(&output, json!({"results":[]}));
    });
}

#[test]
fn symbol_source_returns_exact_lines() {
    let case = Case::new();
    Python::attach(|py| {
        let _fixture = graph_db(py, &case);
        let source = tools(py).getattr("symbol_source").unwrap();
        equal_json(
            &source.call1(("pkg/mod.py::compact_state",)).unwrap(),
            json!({
                "status":"ok","qualified_name":"pkg/mod.py::compact_state","kind":"Function",
                "lines":"4-5","source":"def compact_state(s):\n    \"\"\"Compact the state.\"\"\"\n"
            }),
        );
        let by_name = source.call1(("render",)).unwrap();
        assert!(by_name
            .get_item("qualified_name")
            .unwrap()
            .eq("pkg/mod.py::Preamble.render")
            .unwrap());
        let text: String = by_name.get_item("source").unwrap().extract().unwrap();
        assert!(text.starts_with("    def render(self):"));
        equal_json(
            &source.call1(("nope",)).unwrap(),
            json!({"status":"not_found","query":"nope"}),
        );
        let options = PyDict::new(py);
        options.set_item("max_bytes", 10).unwrap();
        let small = source.call(("compact_state",), Some(&options)).unwrap();
        assert!(small.get_item("truncated").unwrap().is_truthy().unwrap());
        let text: String = small.get_item("source").unwrap().extract().unwrap();
        assert!(text.len() <= 10);
        value_error(py, source.call1((" ",)), "empty");
    });
}

#[test]
fn symbol_source_reports_ambiguity_with_candidates() {
    let case = Case::new();
    Python::attach(|py| {
        let fixture = graph_db(py, &case);
        let sqlite = module(py, "sqlite3");
        let conn = sqlite
            .getattr("connect")
            .unwrap()
            .call1((path(py, &fixture.db),))
            .unwrap();
        let src = case.root().join("pkg/mod.py");
        let name = format!("{}::Other.render", src.display());
        conn.call_method1(
            "execute",
            (
                "INSERT INTO nodes VALUES (?,?,?,?,?,?,?)",
                (
                    name,
                    "Function",
                    "render",
                    "Other",
                    src.to_str().unwrap(),
                    9,
                    10,
                ),
            ),
        )
        .unwrap();
        conn.call_method0("commit").unwrap();
        conn.call_method0("close").unwrap();
        let output = tools(py)
            .getattr("symbol_source")
            .unwrap()
            .call1(("render",))
            .unwrap();
        assert!(output.get_item("status").unwrap().eq("ambiguous").unwrap());
        let candidates = output.get_item("candidates").unwrap();
        let names = module(py, "builtins")
            .getattr("set")
            .unwrap()
            .call0()
            .unwrap();
        for candidate in candidates.try_iter().unwrap() {
            names
                .call_method1(
                    "add",
                    (candidate.unwrap().get_item("qualified_name").unwrap(),),
                )
                .unwrap();
        }
        let expected = module(py, "builtins")
            .getattr("set")
            .unwrap()
            .call1((vec![
                "pkg/mod.py::Preamble.render",
                "pkg/mod.py::Other.render",
            ],))
            .unwrap();
        assert!(names.eq(expected).unwrap());
    });
}

#[test]
fn session_brief_tool_delegates() {
    let _case = Case::new();
    Python::attach(|py| {
        let tools = tools(py);
        let expected = signature(py, &["task", "paths", "agent"], &[]);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let arguments = bound.getattr("arguments")?;
                let task = arguments.get_item("task")?;
                let paths = arguments.get_item("paths")?;
                let agent = arguments.get_item("agent")?;
                Ok(format!("{}|{}|{}", text(&task), text(&paths), text(&agent)))
            })
            .unwrap();
        let _patch = AttrPatch::replace(tools.as_any(), "session_brief", callback.as_any());
        let brief = tools.getattr("session_brief_tool").unwrap();
        assert!(brief
            .call1(("t", vec!["a"], "me"))
            .unwrap()
            .eq("t|['a']|me")
            .unwrap());
        assert!(brief.call1(("t",)).unwrap().eq("t|None|None").unwrap());
    });
}

#[test]
fn workspace_recall_shares_one_embedding_and_compacts() {
    let _case = Case::new();
    Python::attach(|py| {
        let fixture = recall(py);
        let tools = tools(py);
        let options = PyDict::new(py);
        options.set_item("notes_k", 4).unwrap();
        options.set_item("cards_k", 2).unwrap();
        let output = tools
            .getattr("workspace_recall")
            .unwrap()
            .call(("graph skeleton",), Some(&options))
            .unwrap();
        let instruct: String = tools
            .getattr("kb_retrieve")
            .unwrap()
            .getattr("QUERY_INSTRUCT")
            .unwrap()
            .extract()
            .unwrap();
        let expected_call = PyTuple::new(
            py,
            [
                format!("{instruct}graph skeleton")
                    .into_pyobject(py)
                    .unwrap()
                    .into_any(),
                "query".into_pyobject(py).unwrap().into_any(),
            ],
        )
        .unwrap();
        assert!(fixture
            .calls
            .bind(py)
            .eq(PyList::new(py, [expected_call]).unwrap())
            .unwrap());
        let embedders = fixture.embedders.bind(py);
        assert_eq!(embedders.len(), 1);
        let embedded = embedders.get_item(0).unwrap().call1(("anything",)).unwrap();
        equal_json(&embedded, json!([1.0, 0.0]));
        let searches = fixture.searches.bind(py);
        let expected = PyTuple::new(
            py,
            [
                py_json(py, json!([1.0, 0.0])),
                py_json(py, json!(["row"])),
                "M".into_pyobject(py).unwrap().into_any(),
                4_i32.into_pyobject(py).unwrap().into_any(),
            ],
        )
        .unwrap();
        assert!(searches.eq(PyList::new(py, [expected]).unwrap()).unwrap());
        equal_json(
            &output,
            json!({
                "status":"ok",
                "cards":[{"card":"kb_x.md","score":0.457,"snippet":"# Card body"}],
                "memory":[{"source":"notes","path":"research/notes/n.md","title":"N","score":0.512,"snippet":"note text"}]
            }),
        );
    });
}
