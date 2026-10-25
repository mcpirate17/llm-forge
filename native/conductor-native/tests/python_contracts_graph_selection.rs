#![cfg(feature = "python-compat-tests")]
//! Rust-owned graph selection contracts for candidate review.

#[path = "python_contracts/graph_selection_support.rs"]
mod graph_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use graph_support::{context, snapshot, INIT, MODULE, SELECTED, SOURCE};
use pyo3::prelude::*;
use pyo3::types::PyAny;
use support::{module, Case};

const IMPORTS_REEXPORT: &str = "from component_fab.equations import adapt_equation_distribution\ndef test_x():\n    assert adapt_equation_distribution()\n";

fn assert_selected(result: &Bound<'_, PyAny>, relative: &str) {
    let contains: bool = result
        .call_method1("__contains__", (relative,))
        .unwrap()
        .extract()
        .unwrap();
    assert!(contains, "expected selected test {relative}");
}

#[test]
fn candidate_graph_selection_inventories_the_immutable_snapshot() {
    let case = Case::new();
    snapshot(&case, INIT, MODULE, IMPORTS_REEXPORT);
    case.write("tests/test_live_scratch.py", "def test_scratch(): pass\n");
    let database = case.root().join(".code-review-graph/graph.db");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(database).unwrap();
    conn.execute_batch("CREATE TABLE nodes (qualified_name TEXT, file_path TEXT); CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT); CREATE TABLE metadata (key TEXT,value TEXT)").unwrap();
    conn.execute(
        "INSERT INTO metadata VALUES ('git_head_sha',?1)",
        ["c".repeat(40)],
    )
    .unwrap();
    drop(conn);
    Python::attach(|py| {
        let result = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_graph_test_paths")
            .unwrap()
            .call1((context(py, &case), vec![SOURCE]))
            .unwrap();
        let tests = result.get_item(0).unwrap();
        assert_selected(&tests, SELECTED);
        assert!(!tests
            .call_method1("__contains__", ("tests/test_live_scratch.py",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let graph = result.get_item(1).unwrap();
        assert_eq!(
            graph
                .get_item("complete")
                .unwrap()
                .extract::<bool>()
                .unwrap(),
            false
        );
        assert_eq!(
            graph
                .get_item("selection_scope")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "full-test-inventory-fallback"
        );
    });
}
