//! Tests for the production-Rust `.unwrap()` and stub-macro detector.
//!
//! The interesting cases are all about *where* the call sits rather than what
//! it is: the same `.unwrap()` is a defect in a function and an assertion in a
//! test, and the grammar puts the attribute that decides which one it is in a
//! sibling node rather than inside the item.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::rust_scan::{scan_paths, scan_source, RULES};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write `source` to a uniquely named file called `name` and return its path.
fn write_source(name: &str, source: &str) -> PathBuf {
    let nth = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("slop-core-rust-{}-{nth}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    fs::write(&path, source).expect("write sample");
    path
}

/// Every finding for `source`, as (line, rule).
fn scan(source: &str) -> Vec<(usize, String)> {
    scan_source(source, "src/sample.rs")
        .expect("parse")
        .into_iter()
        .map(|finding| (finding.line, finding.rule.to_string()))
        .collect()
}

#[test]
fn an_unwrap_in_production_code_is_reported_at_its_own_line() {
    let findings =
        scan("fn run(v: Vec<u32>) -> u32 {\n    *v\n        .first()\n        .unwrap()\n}\n");
    assert_eq!(findings, vec![(4, "failure/rust-unwrap".to_string())]);
}

#[test]
fn unwrap_err_is_reported_too_because_it_panics_the_same_way() {
    let findings = scan("fn run(r: Result<u32, u32>) -> u32 {\n    r.unwrap_err()\n}\n");
    assert_eq!(findings, vec![(2, "failure/rust-unwrap".to_string())]);
}

#[test]
fn a_call_that_carries_its_reason_is_not_the_panic_this_rule_means() {
    // Two shapes, one guard. `.expect("why")` fails just as loudly and says
    // why, which is the whole of what the rule asks for; and a trait in this
    // tree may define its own `unwrap(self, ctx)`, which carries its context
    // rather than discarding it. Neither is the no-argument `unwrap` in core.
    assert_eq!(
        scan("fn run(r: Option<u32>) -> u32 {\n    r.expect(\"seeded above\")\n}\n"),
        vec![]
    );
    assert_eq!(
        scan("fn run(p: Parser) -> u32 {\n    p.unwrap(&ctx)\n}\n"),
        vec![]
    );
}

#[test]
fn an_unwrap_under_a_cfg_test_module_is_an_assertion_not_a_defect() {
    // A plain helper, so the module attribute is the only thing exempting it.
    let source = "\
#[cfg(test)]
mod tests {
    fn helper() -> u32 {
        Some(0).unwrap()
    }
}
";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn a_cfg_all_test_module_is_still_test_code() {
    // `#[cfg(all(test, unix))]` is as test-only as `#[cfg(test)]`, and is how a
    // platform-specific test module is written.
    let source = "\
#[cfg(all(test, unix))]
mod tests {
    fn helper() -> u32 {
        Some(0).unwrap()
    }
}
";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn an_unwrap_under_a_test_function_is_an_assertion_not_a_defect() {
    // No enclosing `mod`: the attribute is on the function itself, which is how
    // a test written beside the code it covers looks.
    let source = "#[test]\nfn t() {\n    let _ = Some(0).unwrap();\n}\n";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn a_comment_between_the_attribute_and_the_item_does_not_lose_it() {
    // Comments are extras in this grammar, so they arrive as siblings between
    // the attribute and the item it decorates. Treating one as the end of the
    // attribute run would re-arm the rule over everything it was protecting.
    let source = "\
#[cfg(test)]
// A helper the tests below share.
fn helper() -> u32 {
    Some(0).unwrap()
}
";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn an_unwrap_after_a_test_module_is_still_reported() {
    // The attribute run has to be cleared once it is spent, or `#[cfg(test)]`
    // on the first item would exempt every item after it.
    let source = "\
#[cfg(test)]
mod tests {
    fn helper() -> u32 {
        Some(0).unwrap()
    }
}

fn run() -> u32 {
    Some(0).unwrap()
}
";
    assert_eq!(scan(source), vec![(9, "failure/rust-unwrap".to_string())]);
}

#[test]
fn a_written_down_reason_on_the_line_opts_it_out() {
    let source =
        "fn run(v: Vec<u32>) -> u32 {\n    *v.first().unwrap() // guardrail: allow-unwrap\n}\n";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn a_reason_in_the_comment_block_above_opts_it_out() {
    // The marker is two lines up, behind another comment: a lookup that only
    // checks the line directly above would miss it.
    let source = "\
fn run(v: Vec<u32>) -> u32 {
    // guardrail: allow-unwrap
    // `v` is built three lines up in this function and is never empty.
    *v.first().unwrap()
}
";
    assert_eq!(scan(source), vec![]);
}

#[test]
fn a_reason_separated_by_code_does_not_reach_the_call() {
    // The opt-out has to sit against the call it excuses. A marker further up
    // the function would silence every later unwrap in it.
    let source = "\
fn run(v: Vec<u32>) -> u32 {
    // guardrail: allow-unwrap
    let first = v.first().copied().unwrap_or(0);
    *v.last().unwrap() + first
}
";
    assert_eq!(scan(source), vec![(4, "failure/rust-unwrap".to_string())]);
}

#[test]
fn a_stub_macro_is_reported_under_the_dead_rule() {
    assert_eq!(
        scan("fn run() -> u32 {\n    todo!(\"after the reader lands\")\n}\n"),
        vec![(2, "dead/rust-stub".to_string())]
    );
    assert_eq!(
        scan("fn run() -> u32 {\n    unimplemented!()\n}\n"),
        vec![(2, "dead/rust-stub".to_string())]
    );
}

#[test]
fn a_macro_that_is_not_a_stub_is_left_alone() {
    assert_eq!(scan("fn run() {\n    println!(\"done\");\n}\n"), vec![]);
}

#[test]
fn a_test_only_file_is_skipped_whole() {
    // `foo_tests.rs` is reached through a `#[cfg(test)] mod` in another file,
    // so nothing inside it can tell the scanner it is test code.
    let path = write_source(
        "thing_tests.rs",
        "fn helper() -> u32 {\n    Some(0).unwrap()\n}\n",
    );
    let paths = vec![path.to_string_lossy().into_owned()];
    assert_eq!(scan_paths(&paths).expect("scan"), vec![]);
}

#[test]
fn a_file_that_will_not_parse_is_an_error_not_a_skip() {
    let error = scan_source("fn run( {\n", "src/broken.rs").expect_err("must refuse");
    assert!(error.contains("src/broken.rs"), "{error}");
    assert!(error.contains("syntax error"), "{error}");
}

#[test]
fn a_file_that_is_not_there_is_an_error() {
    let paths = vec!["/nonexistent/slop-core/absent.rs".to_string()];
    let error = scan_paths(&paths).expect_err("must refuse");
    assert!(error.contains("cannot read"), "{error}");
}

#[test]
fn rows_arrive_sorted_so_the_caller_can_merge_them() {
    // Both files go in one directory, so the only thing that can order them is
    // the sort: handed [b, a], an unsorted scanner returns [b, a].
    let unwrap = "fn run() -> u32 {\n    Some(0).unwrap()\n}\n";
    let nth = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("slop-core-order-{}-{nth}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let mut paths = Vec::new();
    for name in ["b.rs", "a.rs"] {
        let path = dir.join(name);
        fs::write(&path, unwrap).expect("write sample");
        paths.push(path.to_string_lossy().into_owned());
    }
    let rows = scan_paths(&paths).expect("scan");
    let names: Vec<String> = rows
        .iter()
        .map(|row| row.path.rsplit('/').next().unwrap_or(&row.path).to_string())
        .collect();
    assert_eq!(names, vec!["a.rs".to_string(), "b.rs".to_string()]);
}

#[test]
fn the_published_rules_are_the_ones_the_scanner_emits() {
    let unwrap = scan("fn run() -> u32 {\n    Some(0).unwrap()\n}\n");
    let stub = scan("fn run() -> u32 {\n    todo!()\n}\n");
    let emitted: Vec<String> = unwrap
        .into_iter()
        .chain(stub)
        .map(|(_, rule)| rule)
        .collect();
    assert_eq!(
        emitted,
        RULES.iter().map(|r| r.to_string()).collect::<Vec<_>>()
    );
}
