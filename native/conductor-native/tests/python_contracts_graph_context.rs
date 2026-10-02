#![cfg(feature = "python-compat-tests")]
//! Rust-owned Python graph-context contracts, including AST and CLI behavior.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, AttrPatch, Case};

const GRAPH_SCHEMA: &str = "
CREATE TABLE nodes (
    id INTEGER PRIMARY KEY, kind TEXT, name TEXT, qualified_name TEXT,
    file_path TEXT, line_start INTEGER, line_end INTEGER, language TEXT,
    parent_name TEXT, params TEXT, return_type TEXT, modifiers TEXT,
    is_test INTEGER DEFAULT 0, file_hash TEXT, extra TEXT DEFAULT '{}',
    updated_at REAL DEFAULT 0.0, signature TEXT, community_id INTEGER DEFAULT 0
);
CREATE TABLE edges (
    id INTEGER PRIMARY KEY, kind TEXT, source_qualified TEXT,
    target_qualified TEXT, file_path TEXT, line INTEGER DEFAULT 0,
    extra TEXT DEFAULT '{}', updated_at REAL DEFAULT 0.0,
    confidence REAL DEFAULT 1.0, confidence_tier TEXT DEFAULT 'EXTRACTED'
);";

#[test]
fn rust_adapter_and_content_cache_preserve_current_signatures_after_same_size_edit() {
    let case = Case::new();
    let repo = context_repo(&case);
    let source = case.write(
        "ctx_ws/src/engine.rs",
        "pub fn run(first: u8) -> u8 { first }\n",
    );
    let stamp = source.metadata().unwrap().modified().unwrap();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let options = PyDict::new(py);
        options.set_item("with_graph", false).unwrap();
        let get = graph.getattr("get_file_context").unwrap();
        let first = get
            .call((path(py, &repo), "src/engine.rs"), Some(&options))
            .unwrap();
        assert_eq!(
            first
                .getattr("language")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "rust"
        );
        assert!(first
            .getattr("skeleton")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("first: u8"));
        let again = get
            .call((path(py, &repo), "src/engine.rs"), Some(&options))
            .unwrap();
        assert!(first
            .getattr("skeleton")
            .unwrap()
            .eq(again.getattr("skeleton").unwrap())
            .unwrap());
        fs::write(&source, "pub fn run(other: u8) -> u8 { other }\n").unwrap();
        fs::File::open(&source)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(stamp))
            .unwrap();
        let edited = get
            .call((path(py, &repo), "src/engine.rs"), Some(&options))
            .unwrap();
        let skeleton: String = edited.getattr("skeleton").unwrap().extract().unwrap();
        assert!(skeleton.contains("other: u8"));
        assert!(!skeleton.contains("first: u8"));
        let markdown: String = graph
            .getattr("format_markdown_context")
            .unwrap()
            .call1((edited,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(markdown.contains("```rust"));
    });
}

fn context_repo(case: &Case) -> PathBuf {
    let repo = case.mkdir("ctx_ws");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "graph-tester"],
        vec!["config", "user.email", "graph@example.com"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    case.write("ctx_ws/CONVENTIONS.md", "# Graph Context Test Fixture\n");
    assert!(Command::new("git")
        .args(["add", "CONVENTIONS.md"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "init-graph-context", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    repo
}

fn database(repo: &Path) -> Connection {
    let file = repo.join(".code-review-graph/graph.db");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let conn = Connection::open(file).unwrap();
    conn.execute_batch(GRAPH_SCHEMA).unwrap();
    conn
}

fn skeleton(
    graph: &Bound<'_, PyModule>,
    code: &str,
    target: Option<&str>,
) -> (String, Vec<String>) {
    graph
        .getattr("extract_ast_skeleton")
        .unwrap()
        .call1((code, target))
        .unwrap()
        .extract()
        .unwrap()
}

fn names(py: Python<'_>, values: &[Py<PyAny>]) -> Vec<String> {
    values
        .iter()
        .map(|item| {
            item.bind(py)
                .getattr("qualified_name")
                .unwrap()
                .extract()
                .unwrap()
        })
        .collect()
}

fn relationships(
    py: Python<'_>,
    graph: &Bound<'_, PyModule>,
    repo: &Path,
    file: &str,
    target: Option<&str>,
) -> (Vec<Py<PyAny>>, Vec<Py<PyAny>>, String) {
    graph
        .getattr("query_graph_relationships")
        .unwrap()
        .call1((path(py, repo), file, target))
        .unwrap()
        .extract()
        .unwrap()
}

fn relationship(graph: &Bound<'_, PyModule>, qualified: &str, kind: &str, file: &str) -> Py<PyAny> {
    graph
        .getattr("GraphRelationship")
        .unwrap()
        .call1((qualified, kind, file))
        .unwrap()
        .unbind()
}

fn summary<'py>(
    graph: &Bound<'py, PyModule>,
    file: &str,
    skeleton: &str,
    symbols: Vec<&str>,
    callers: Vec<Py<PyAny>>,
    callees: Vec<Py<PyAny>>,
    status: &str,
) -> Bound<'py, PyAny> {
    graph
        .getattr("FileContextSummary")
        .unwrap()
        .call1((file, skeleton, symbols, callers, callees, status))
        .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = py
        .import("io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(&py.import("sys").unwrap(), stream, &buffer);
    (buffer, patch)
}

#[test]
fn extract_ast_skeleton_functions() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let code = "import os\nfrom typing import Optional\n\ndef calculate_total(price: float, tax: float = 0.05) -> float:\n    \"\"\"Compute total with tax.\"\"\"\n    subtotal = price * 1.0\n    return subtotal + (subtotal * tax)\n";
        let (stub, symbols) = skeleton(&graph, code, None);
        for text in [
            "import os",
            "def calculate_total(",
            ") -> float:",
            "\"\"\"Compute total with tax.\"\"\"",
            "...",
        ] {
            assert!(stub.contains(text), "missing {text:?} in {stub}");
        }
        assert!(!stub.contains("subtotal = price"));
        assert!(symbols.contains(&"calculate_total".to_owned()));
    });
}

#[test]
fn extract_ast_skeleton_classes() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let code = "class ModelTrainer:\n    \"\"\"Trainer engine.\"\"\"\n    batch_size: int = 32\n\n    def train_step(self, x: int) -> bool:\n        \"\"\"One step.\"\"\"\n        y = x * 2\n        return True\n";
        let (stub, symbols) = skeleton(&graph, code, None);
        for text in [
            "class ModelTrainer:",
            "\"\"\"Trainer engine.\"\"\"",
            "def train_step(self, x: int) -> bool:",
            "\"\"\"One step.\"\"\"",
            "...",
        ] {
            assert!(stub.contains(text), "missing {text:?} in {stub}");
        }
        assert!(!stub.contains("y = x * 2"));
        assert!(symbols.contains(&"ModelTrainer".to_owned()));
    });
}

#[test]
fn extract_ast_skeleton_filter_target_symbol() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let code = "def func_a():\n    return 1\n\ndef func_b():\n    return 2\n";
        let (stub, symbols) = skeleton(&graph, code, Some("func_b"));
        assert!(stub.contains("func_b"));
        assert!(!stub.contains("func_a"));
        assert!(symbols.contains(&"func_a".to_owned()));
        assert!(symbols.contains(&"func_b".to_owned()));
    });
}

#[test]
fn query_graph_relationships() {
    let case = Case::new();
    let repo = context_repo(&case);
    let resolved_repo = repo.canonicalize().unwrap();
    let conn = database(&repo);
    for (qualified, name, file) in [
        (
            "pkg.engine.run",
            "run",
            resolved_repo
                .join("pkg/engine.py")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        (
            "pkg.caller.invoke",
            "invoke",
            resolved_repo
                .join("pkg/caller.py")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        ("pkg.sub.helper", "helper", "pkg/sub.py".to_owned()),
    ] {
        conn.execute(
            "INSERT INTO nodes (qualified_name, name, file_path) VALUES (?1, ?2, ?3)",
            params![qualified, name, file],
        )
        .unwrap();
    }
    for (source, target) in [
        ("pkg.caller.invoke", "pkg.engine.run"),
        ("pkg.engine.run", "pkg.sub.helper"),
    ] {
        conn.execute(
            "INSERT INTO edges (source_qualified, target_qualified, kind) VALUES (?1, ?2, 'calls')",
            params![source, target],
        )
        .unwrap();
    }
    drop(conn);
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let (callers, callees, status) = relationships(py, &graph, &repo, "pkg/engine.py", None);
        assert_eq!(status, "ok");
        assert_eq!(names(py, &callers), ["pkg.caller.invoke"]);
        assert_eq!(names(py, &callees), ["pkg.sub.helper"]);
    });
}

#[test]
fn get_file_context_full_flow() {
    let case = Case::new();
    let repo = context_repo(&case);
    case.write(
        "ctx_ws/pkg/mod.py",
        "def ping() -> str:\n    return 'pong'\n",
    );
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let kw = pyo3::types::PyDict::new(py);
        kw.set_item("with_graph", false).unwrap();
        let ctx = graph
            .getattr("get_file_context")
            .unwrap()
            .call((path(py, &repo), "pkg/mod.py"), Some(&kw))
            .unwrap();
        assert_eq!(
            ctx.getattr("file_path")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "pkg/mod.py"
        );
        assert!(ctx
            .getattr("skeleton")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("def ping() -> str:"));
        assert!(ctx
            .getattr("symbols")
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap()
            .contains(&"ping".to_owned()));
        assert!(ctx
            .getattr("callers")
            .unwrap()
            .eq(PyList::empty(py))
            .unwrap());
        let md: String = graph
            .getattr("format_markdown_context")
            .unwrap()
            .call1((&ctx,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(md.contains("### AST Context: `pkg/mod.py`"));
        assert!(md.contains("def ping() -> str:"));
    });
}

#[test]
fn format_markdown_context_with_relationships() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let ctx = summary(
            &graph,
            "foo.py",
            "def foo(): ...",
            vec!["foo"],
            vec![relationship(&graph, "bar.caller", "calls", "bar.py")],
            vec![relationship(&graph, "baz.callee", "calls", "baz.py")],
            "unavailable (graph.db missing)",
        );
        let md: String = graph
            .getattr("format_markdown_context")
            .unwrap()
            .call1((&ctx,))
            .unwrap()
            .extract()
            .unwrap();
        for expected in [
            "Called By (Inbound Call Sites)",
            "bar.caller",
            "Calls (Outbound Dependencies)",
            "baz.callee",
            "Notice: code-review-graph unavailable",
        ] {
            assert!(md.contains(expected), "missing {expected:?} in {md}");
        }
    });
}

#[test]
fn main_cli() {
    let case = Case::new();
    let repo = context_repo(&case);
    case.write(
        "ctx_ws/pkg/cli_test.py",
        "def cli_func(x: int) -> int:\n    return x + 1\n",
    );
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let (output, _patch) = capture(py, "stdout");
        let code: i32 = graph
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo",
                repo.to_str().unwrap(),
                "--no-graph",
                "--json",
                "pkg/cli_test.py",
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let rendered: String = output.call_method0("getvalue").unwrap().extract().unwrap();
        let data: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(data["file_path"], "pkg/cli_test.py");
        assert!(data["skeleton"]
            .as_str()
            .unwrap()
            .contains("def cli_func(x: int) -> int:"));
        assert!(data["symbols"]
            .as_array()
            .unwrap()
            .contains(&Value::String("cli_func".into())));
    });
}

#[test]
fn error_handling_missing_file_and_syntax_error() {
    let case = Case::new();
    let repo = context_repo(&case);
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let class = graph.getattr("GraphContextError").unwrap();
        assert_error(
            py,
            graph
                .getattr("get_file_context")
                .unwrap()
                .call1((path(py, &repo), "nonexistent.py"))
                .unwrap_err(),
            &class,
            "file not found",
        );
        assert_error(
            py,
            graph
                .getattr("extract_ast_skeleton")
                .unwrap()
                .call1(("def broken_syntax(:\n",))
                .unwrap_err(),
            &class,
            "syntax error in source",
        );
    });
}

#[test]
fn extract_ast_skeleton_async_function() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let code = "async def fetch_data(url: str) -> dict:\n    \"\"\"Fetch async.\"\"\"\n    res = await get(url)\n    return res\n\nasync def other():\n    pass\n";
        let (stub, symbols) = skeleton(&graph, code, Some("fetch_data"));
        assert!(stub.contains("async def fetch_data("));
        assert!(stub.contains("\"\"\"Fetch async.\"\"\""));
        assert!(!stub.contains("async def other"));
        assert!(symbols.contains(&"fetch_data".to_owned()));
        assert!(symbols.contains(&"other".to_owned()));
    });
}

#[test]
fn extract_ast_skeleton_class_method_filtering() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let code = "class Service:\n    def method_a(self):\n        return 1\n    def method_b(self):\n        return 2\n\nclass OtherService:\n    def method_c(self):\n        return 3\n";
        let (stub, _) = skeleton(&graph, code, Some("method_a"));
        assert!(stub.contains("class Service:"));
        assert!(stub.contains("def method_a("));
        assert!(!stub.contains("def method_b("));
        assert!(!stub.contains("class OtherService:"));
    });
}

#[test]
fn graph_relationship_immutability() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let rel = relationship(&graph, "pkg.foo", "calls", "pkg/foo.py");
        let frozen = py
            .import("dataclasses")
            .unwrap()
            .getattr("FrozenInstanceError")
            .unwrap();
        assert_error(
            py,
            rel.bind(py).setattr("qualified_name", "other").unwrap_err(),
            &frozen,
            "cannot assign",
        );
    });
}

#[test]
fn query_graph_with_target_symbol() {
    let case = Case::new();
    let repo = context_repo(&case);
    let conn = database(&repo);
    let source = repo
        .canonicalize()
        .unwrap()
        .join("pkg/engine.py")
        .to_str()
        .unwrap()
        .to_owned();
    conn.execute(
        "INSERT INTO nodes (qualified_name, name, file_path) VALUES ('pkg.engine.run', 'run', ?1)",
        params![source],
    )
    .unwrap();
    conn.execute("INSERT INTO nodes (qualified_name, name, file_path) VALUES ('pkg.caller.invoke', 'invoke', 'pkg/caller.py')", []).unwrap();
    conn.execute("INSERT INTO edges (source_qualified, target_qualified, kind) VALUES ('pkg.caller.invoke', 'pkg.engine.run', 'calls')", []).unwrap();
    drop(conn);
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let (callers, _, status) = relationships(py, &graph, &repo, "pkg/engine.py", Some("run"));
        assert_eq!(status, "ok");
        assert_eq!(names(py, &callers), ["pkg.caller.invoke"]);
        let result = graph
            .getattr("query_graph_relationships")
            .unwrap()
            .call1((path(py, &repo), "pkg/engine.py", "nonexistent"))
            .unwrap();
        assert!(result.get_item(0).unwrap().eq(PyList::empty(py)).unwrap());
    });
}

#[test]
fn find_syntactic_callers() {
    let case = Case::new();
    let repo = context_repo(&case);
    case.write("ctx_ws/pkg/callee.py", "def target_op(): pass\n");
    case.write(
        "ctx_ws/pkg/caller.py",
        "from pkg.callee import target_op\ntarget_op()\n",
    );
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let result = graph
            .getattr("find_syntactic_callers")
            .unwrap()
            .call1((path(py, &repo), "target_op", "pkg/callee.py"))
            .unwrap();
        let callers: Vec<Py<PyAny>> = result.extract().unwrap();
        assert!(callers.iter().any(|caller| caller
            .bind(py)
            .getattr("file_path")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("pkg/caller.py")));
    });
}

#[test]
fn unknown_symbol_raises_error() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let error = graph
            .getattr("extract_ast_skeleton")
            .unwrap()
            .call1(("def foo(): pass\n", "bar"))
            .unwrap_err();
        assert_error(
            py,
            error,
            &graph.getattr("GraphContextError").unwrap(),
            "symbol 'bar' not found",
        );
    });
}

#[test]
fn extract_ast_skeleton_preserves_assign_constants() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let (stub, _) = skeleton(
            &graph,
            "MAX_RETRIES = 5\nCONFIG_SPEC = {'a': 1, 'b': 2}\n\ndef run():\n    return 1\n",
            None,
        );
        for expected in ["MAX_RETRIES = 5", "CONFIG_SPEC = ...", "def run():"] {
            assert!(stub.contains(expected));
        }
    });
}

#[test]
fn main_cli_error_path() {
    let case = Case::new();
    let repo = context_repo(&case);
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let (error, _patch) = capture(py, "stderr");
        let code: i32 = graph
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo",
                repo.to_str().unwrap(),
                "nonexistent_file.py",
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
        let rendered: String = error.call_method0("getvalue").unwrap().extract().unwrap();
        assert!(rendered.contains("graph-context ERROR: file not found"));
    });
}

#[test]
fn is_test_path_discriminates_test_files() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let classifier = graph.getattr("_is_test_path").unwrap();
        for (file, expected) in [
            ("conductor/test_graph_context.py", true),
            ("research/tests/helpers.py", true),
            ("pkg/util_test.py", true),
            ("conductor/mutation_testing.py", false),
            ("research/tools/attestation.py", false),
            ("contest/protest.py", false),
        ] {
            assert_eq!(
                classifier
                    .call1((file,))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                expected,
                "{file}"
            );
        }
    });
}

#[test]
fn format_markdown_binning_survives_test_substring_in_name() {
    let _case = Case::new();
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_context");
        let ctx = summary(
            &graph,
            "conductor/mutation_testing.py",
            "def run_campaign(): ...",
            vec!["run_campaign"],
            vec![
                relationship(
                    &graph,
                    "conductor/mutation_testing.py::_apply_mutation",
                    "CALLS",
                    "conductor/mutation_testing.py",
                ),
                relationship(
                    &graph,
                    "conductor/test_mutation_testing.py:72",
                    "TESTED_BY",
                    "conductor/test_mutation_testing.py",
                ),
            ],
            vec![
                relationship(
                    &graph,
                    "audit/orchestrator/snapshot_worktree.py::isolated_snapshot",
                    "CALLS",
                    "audit/orchestrator/snapshot_worktree.py",
                ),
                relationship(
                    &graph,
                    "conductor/test_graph_context.py::context_repo",
                    "CALLS",
                    "conductor/test_graph_context.py",
                ),
            ],
            "ok",
        );
        let md: String = graph
            .getattr("format_markdown_context")
            .unwrap()
            .call1((ctx,))
            .unwrap()
            .extract()
            .unwrap();
        let called_by = md
            .split("**Called By")
            .nth(1)
            .unwrap()
            .split("**Calls")
            .next()
            .unwrap();
        let calls = md
            .split("**Calls")
            .nth(1)
            .unwrap()
            .split("**Tested By")
            .next()
            .unwrap();
        let tested_by = md.split("**Tested By").nth(1).unwrap();
        assert!(called_by.contains("_apply_mutation"));
        assert!(calls.contains("isolated_snapshot"));
        assert!(tested_by.contains("test_mutation_testing.py:72"));
        assert!(tested_by.contains("test_graph_context.py::context_repo"));
        assert!(!tested_by.contains("_apply_mutation"));
    });
}
