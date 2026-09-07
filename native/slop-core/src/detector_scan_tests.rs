//! Tests for the gate-facing half of the silent-fallback detector.
//!
//! The classifier itself is the repository audit's, and the audit's own tests
//! cover it. What is new here is the door it is reached through, so these
//! tests pin the two things the gate needs and the audit does not: an opt-out
//! a reviewer can write down, and a refusal on a file that will not parse.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::detector_scan::fallback_scan_files;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write `source` to a uniquely named file and return its path.
fn write_source(source: &str) -> PathBuf {
    let nth = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("slop-core-fallback-{}-{nth}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("sample.py");
    fs::write(&path, source).expect("write sample");
    path
}

/// Run `f` with the interpreter attached, starting it if this is the first test
/// to need it. `cargo test` links libpython but does not start it, and the
/// tests run in parallel, so every entry point has to be able to be the first.
fn attached<R>(f: impl for<'py> FnOnce(Python<'py>) -> R) -> R {
    Python::initialize();
    Python::attach(f)
}

/// Every row `fallback_scan_files` reports for `sources`, as (path, line, rule, message).
fn scan(sources: &[&str]) -> Vec<(String, usize, String, String)> {
    let paths: Vec<String> = sources
        .iter()
        .map(|source| write_source(source).to_string_lossy().into_owned())
        .collect();
    attached(|py| {
        let rows: Py<PyList> = fallback_scan_files(py, paths).expect("scan");
        rows.bind(py)
            .iter()
            .map(|row| {
                (
                    row.get_item("path").unwrap().extract().unwrap(),
                    row.get_item("line").unwrap().extract().unwrap(),
                    row.get_item("rule").unwrap().extract().unwrap(),
                    row.get_item("message").unwrap().extract().unwrap(),
                )
            })
            .collect()
    })
}

#[test]
fn a_swallowed_error_is_reported_at_the_handler_line() {
    let rows = scan(&["try:\n    risky()\nexcept ValueError:\n    pass\n"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, 3);
    assert_eq!(rows[0].2, "failure/silent-fallback");
    assert!(
        rows[0]
            .3
            .starts_with("except ValueError swallows the error"),
        "message names the exception: {}",
        rows[0].3
    );
}

#[test]
fn a_written_down_reason_opts_the_handler_out() {
    let rows = scan(&[
        "try:\n    risky()\nexcept ValueError:\n    # guardrail: allow-fallback -- absent is fine\n    pass\n",
    ]);
    assert!(
        rows.is_empty(),
        "opted-out handler still reported: {rows:?}"
    );
}

#[test]
fn the_opt_out_reaches_the_except_line_itself() {
    let rows =
        scan(&["try:\n    risky()\nexcept ValueError:  # guardrail: allow-fallback\n    pass\n"]);
    assert!(
        rows.is_empty(),
        "opt-out on the except line ignored: {rows:?}"
    );
}

#[test]
fn an_opt_out_below_the_handler_does_not_reach_it() {
    let rows =
        scan(&["try:\n    risky()\nexcept ValueError:\n    pass\n\n# guardrail: allow-fallback\n"]);
    assert_eq!(rows.len(), 1, "a comment past the handler silenced it");
}

#[test]
fn a_file_that_will_not_parse_is_an_error_not_a_skip() {
    let path = write_source("def broken(:\n    pass\n");
    let raw = path.to_string_lossy().into_owned();
    let failed = attached(|py| fallback_scan_files(py, vec![raw]).is_err());
    assert!(failed, "an unparsable file passed the gate silently");
}

#[test]
fn a_file_that_is_not_there_is_an_error() {
    let missing = std::env::temp_dir().join("slop-core-fallback-absent/none.py");
    let raw = missing.to_string_lossy().into_owned();
    let failed = attached(|py| fallback_scan_files(py, vec![raw]).is_err());
    assert!(failed, "an unreadable file passed the gate silently");
}

#[test]
fn a_narrow_handler_that_returns_a_default_is_not_a_fallback() {
    let rows = scan(&[
        "def f():\n    try:\n        return risky()\n    except ValueError:\n        return 0\n",
    ]);
    assert!(
        rows.is_empty(),
        "a named exception returning a default was reported: {rows:?}"
    );
}

#[test]
fn rows_arrive_sorted_so_the_caller_can_merge_them() {
    let nth = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("slop-core-sorted-{}-{nth}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let swallow = "try:\n    risky()\nexcept ValueError:\n    pass\n";
    let mut paths = Vec::new();
    // Handed to the scanner in the order that is not the answer, so a caller
    // merging these against another sorted list gets one pass down the tree.
    for name in ["b.py", "a.py"] {
        let path = dir.join(name);
        fs::write(&path, swallow).expect("write sample");
        paths.push(path.to_string_lossy().into_owned());
    }
    let rows: Vec<String> = attached(|py| {
        let list: Py<PyList> = fallback_scan_files(py, paths).expect("scan");
        list.bind(py)
            .iter()
            .map(|row| row.get_item("path").unwrap().extract().unwrap())
            .collect()
    });
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ends_with("a.py"), "unsorted: {rows:?}");
    assert!(rows[1].ends_with("b.py"), "unsorted: {rows:?}");
}
