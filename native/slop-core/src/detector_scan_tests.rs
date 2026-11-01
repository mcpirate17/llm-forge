//! Tests for the gate-facing half of the silent-fallback detector.
//!
//! The classifier itself is the repository audit's, and the audit's own tests
//! cover it. What is new here is the door it is reached through, so these
//! tests pin the two things the gate needs and the audit does not: an opt-out
//! a reviewer can write down, and a refusal on a file that will not parse.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::detector_scan::{inspect, scan_paths, scan_source, Policy};
use std::collections::BTreeSet;

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

/// Every row `fallback_scan_files` reports for `sources`, as (path, line, rule, message).
fn scan(sources: &[&str]) -> Vec<(String, usize, String, String)> {
    let paths: Vec<String> = sources
        .iter()
        .map(|source| write_source(source).to_string_lossy().into_owned())
        .collect();
    let rows = scan_paths(&paths)
        .expect("scan")
        .into_iter()
        .map(|row| (row.path, row.line, row.rule.to_owned(), row.message))
        .collect();
    for path in paths {
        fs::remove_dir_all(PathBuf::from(path).parent().unwrap()).unwrap();
    }
    rows
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
    let failed = scan_paths(&[raw]).is_err();
    assert!(failed, "an unparsable file passed the gate silently");
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_file_that_is_not_there_is_an_error() {
    let missing = std::env::temp_dir().join("slop-core-fallback-absent/none.py");
    let raw = missing.to_string_lossy().into_owned();
    let failed = scan_paths(&[raw]).is_err();
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
    let rows: Vec<String> = scan_paths(&paths)
        .unwrap()
        .into_iter()
        .map(|row| row.path)
        .collect();
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ends_with("a.py"), "unsorted: {rows:?}");
    assert!(rows[1].ends_with("b.py"), "unsorted: {rows:?}");
}

#[test]
fn broad_handlers_require_silent_control_or_defaults() {
    for body in ["pass", "return 0", "x = None"] {
        let source = format!("def f():\n    try:\n        risky()\n    except (OSError, Exception):\n        {body}\n");
        let rows = scan_source("a.py", &source).unwrap();
        assert_eq!(rows.len(), 1, "{source}");
        assert!(rows[0].message.contains("(OSError, Exception)"));
    }
    for body in [
        "raise",
        "return fallback()",
        "log.error('failed')",
        "x: int = 0",
        "if True:\n            return 0",
    ] {
        let source =
            format!("def f():\n    try:\n        risky()\n    except Exception:\n        {body}\n");
        assert!(scan_source("a.py", &source).unwrap().is_empty(), "{source}");
    }
}

#[test]
fn async_nested_and_exception_group_handlers_are_inspected() {
    let source = "async def café():\n    try:\n        await risky()\n    except* Exception as e:\n        pass\n";
    let rows = scan_source("a.py", source).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line, 4);
    assert!(rows[0].message.contains("except Exception"));
    for ending in ["\r\n", "\r"] {
        let rows = scan_source("a.py", &source.replace('\n', ending)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, 4);
    }
}

#[test]
fn audit_keeps_file_findings_but_marks_unparsed_ast_explicitly() {
    let allowed = BTreeSet::new();
    let policy = Policy {
        allowed_files: &allowed,
        allowed_functions: &allowed,
        god_file_lines: 1,
        god_func_lines: 1,
    };
    let result = inspect("broken.py", "def broken(:\n pass\n", &policy).unwrap();
    assert_eq!(result.god_files.len(), 1);
    assert_eq!(result.unparsable, ["broken.py"]);
    assert!(result.god_functions.is_empty());
    assert!(scan_source("broken.py", "def broken(:\n pass\n").is_err());
}

#[test]
fn structural_policy_respects_exact_function_allowlist_and_registration_shape() {
    let allowed = BTreeSet::from(["a.py::ok".to_owned()]);
    let policy = Policy {
        allowed_files: &allowed,
        allowed_functions: &allowed,
        god_file_lines: 100,
        god_func_lines: 1,
    };
    let source = "def ok():\n    pass\ndef long():\n    pass\ndef register_route():\n    def handler():\n        pass\n";
    let result = inspect("a.py", source, &policy).unwrap();
    assert_eq!(
        result.god_functions,
        ["a.py:3 long (2)", "a.py:6 handler (2)"]
    );
    assert!(result.unparsable.is_empty());
}

#[test]
fn detector_rejects_oversized_nul_and_non_utf8_inputs() {
    assert!(scan_source("a.py", &"#".repeat(1024 * 1024 + 1)).is_err());
    assert!(scan_source("a.py", "x='\0'").is_err());
    let path = write_source("");
    fs::write(&path, [0xff]).unwrap();
    assert!(scan_paths(&[path.to_string_lossy().into_owned()]).is_err());
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn detector_file_adapter_rejects_links_and_fifos_without_blocking() {
    let source = write_source("x=1\n");
    let dir = source.parent().unwrap();
    let link = dir.join("link.py");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(crate::detector_scan::read_source(&link).is_err());
    let fifo = dir.join("fifo.py");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(crate::detector_scan::read_source(&fifo).is_err());
    fs::remove_dir_all(dir).unwrap();
}
