//! The backlog's contract: a run speaks only for what it measured, so what it did not
//! measure is neither fixed nor lost.

use crate::backlog::backlog;
use crate::gate::{Measured, Summary};
use crate::index_tests::Tree;
use serde_json::{json, Value};
use std::path::PathBuf;

fn finding(module: &str, qualname: &str, verdict: &str, rule: &str) -> Value {
    json!({
        "module": module,
        "qualname": qualname,
        "verdict": verdict,
        "rule": rule,
        "lineno": 3,
        "description": format!("{rule} removed"),
    })
}

fn whole(module: &str) -> Measured {
    Measured {
        module: module.into(),
        units: None,
    }
}

struct Ledger {
    tree: Tree,
    runs: usize,
}

impl Ledger {
    fn new(tag: &str) -> Ledger {
        Ledger {
            tree: Tree::new(&format!("backlog-{tag}")),
            runs: 0,
        }
    }

    fn ledger(&self) -> PathBuf {
        self.tree.path().join("ledger.json")
    }

    fn report(&self) -> PathBuf {
        self.tree.path().join("reports/backlog.md")
    }

    fn fold(&mut self, summary: Summary, dry_run: bool) -> String {
        self.runs += 1;
        let path = self.tree.path().join(format!("run-{}.json", self.runs));
        std::fs::write(&path, serde_json::to_string(&summary).unwrap()).unwrap();
        let paths = [path.to_str().unwrap().to_owned()];
        backlog(&paths, &self.ledger(), &self.report(), dry_run).unwrap()
    }

    fn items(&self) -> Vec<(String, String, String)> {
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(self.ledger()).unwrap()).unwrap();
        stored["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                let field = |key: &str| item[key].as_str().unwrap().to_owned();
                (field("module"), field("qualname"), field("verdict"))
            })
            .collect()
    }
}

fn first_sweep(ledger: &mut Ledger) -> String {
    ledger.fold(
        Summary {
            measured: vec![whole("m.py"), whole("other.py")],
            blocking: vec![finding("m.py", "f", "REACHABLE_BUT_UNTESTED", "drop_clamp")],
            advisory: vec![
                finding("m.py", "g", "NO_DIFFERENCE_OBSERVED", "drop_clamp"),
                finding("m.py", "g", "NO_DIFFERENCE_OBSERVED", "drop_where"),
            ],
            untested: vec![finding("other.py", "h", "NOT_EXERCISED", "drop_clamp")],
            ..Summary::default()
        },
        false,
    )
}

fn item(module: &str, qualname: &str, verdict: &str) -> (String, String, String) {
    (module.into(), qualname.into(), verdict.into())
}

#[test]
fn a_first_sweep_is_all_new_and_collapses_findings_to_functions() {
    let mut ledger = Ledger::new("first");
    let said = first_sweep(&mut ledger);
    assert!(
        said.starts_with("4 findings -> 3 items (3 new, 0 carried, 0 fixed)"),
        "{said}"
    );
    let report = std::fs::read_to_string(ledger.report()).unwrap();
    assert!(report.contains("## New (3)"), "{report}");
    assert!(
        report.contains("| REACHABLE_BUT_UNTESTED | 1 | 0 | 0 |"),
        "{report}"
    );
    assert!(
        report.contains("`m.py:3` **g** — removing it changed nothing"),
        "{report}"
    );
    assert!(report.contains("drop_clamp, drop_where"), "{report}");
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(ledger.ledger()).unwrap()).unwrap();
    assert_eq!(stored["schema_version"], 2);
    assert_eq!(stored["scope"], json!(["m.py", "other.py"]));
}

#[test]
fn a_clean_measured_module_fixes_what_it_had() {
    let mut ledger = Ledger::new("fixed");
    first_sweep(&mut ledger);
    let said = ledger.fold(
        Summary {
            measured: vec![whole("m.py")],
            advisory: vec![finding("m.py", "g", "NO_DIFFERENCE_OBSERVED", "drop_clamp")],
            ..Summary::default()
        },
        false,
    );
    assert!(
        said.starts_with("1 findings -> 1 items (0 new, 1 carried, 1 fixed)"),
        "{said}"
    );
    let report = std::fs::read_to_string(ledger.report()).unwrap();
    assert!(
        report.contains("- `m.py` **f** — REACHABLE_BUT_UNTESTED"),
        "{report}"
    );
    assert_eq!(
        ledger.items(),
        [
            item("m.py", "g", "NO_DIFFERENCE_OBSERVED"),
            item("other.py", "h", "NOT_EXERCISED")
        ]
    );
}

#[test]
fn a_run_over_some_units_speaks_only_for_them() {
    let mut ledger = Ledger::new("units");
    first_sweep(&mut ledger);
    let units = Some(vec!["g".to_owned()]);
    let said = ledger.fold(
        Summary {
            measured: vec![Measured {
                module: "m.py".into(),
                units,
            }],
            ..Summary::default()
        },
        false,
    );
    assert!(
        said.starts_with("0 findings -> 0 items (0 new, 0 carried, 1 fixed)"),
        "{said}"
    );
    assert_eq!(
        ledger.items(),
        [
            item("m.py", "f", "REACHABLE_BUT_UNTESTED"),
            item("other.py", "h", "NOT_EXERCISED")
        ]
    );
}

#[test]
fn a_function_the_run_could_not_measure_is_neither_fixed_nor_dropped() {
    let mut ledger = Ledger::new("unknown");
    first_sweep(&mut ledger);
    let mut unreported = finding("m.py", "f", "UNREPORTED", "probe-unit-unreported");
    unreported["drivers_source"] = json!("record");
    let said = ledger.fold(
        Summary {
            measured: vec![whole("m.py")],
            incomplete: vec![unreported],
            ..Summary::default()
        },
        false,
    );
    assert!(said.contains("1 fixed"), "{said}");
    assert!(ledger
        .items()
        .contains(&item("m.py", "f", "REACHABLE_BUT_UNTESTED")));
    assert!(!ledger
        .items()
        .contains(&item("m.py", "g", "NO_DIFFERENCE_OBSERVED")));
}

#[test]
fn a_dry_run_writes_nothing_and_returns_the_report() {
    let mut ledger = Ledger::new("dry");
    let report = ledger.fold(
        Summary {
            measured: vec![whole("m.py")],
            blocking: vec![finding("m.py", "f", "REACHABLE_BUT_UNTESTED", "drop_clamp")],
            ..Summary::default()
        },
        true,
    );
    assert!(
        report.starts_with("# Equivalence probe backlog"),
        "{report}"
    );
    assert!(!ledger.ledger().exists() && !ledger.report().exists());
}

#[test]
fn nothing_new_and_nothing_fixed_says_so() {
    let mut ledger = Ledger::new("quiet");
    first_sweep(&mut ledger);
    first_sweep(&mut ledger);
    let report = std::fs::read_to_string(ledger.report()).unwrap();
    assert!(report.contains("## Carried (3)"), "{report}");
    assert!(
        report.contains("Nothing new and nothing fixed since the last sweep."),
        "{report}"
    );
}

#[test]
fn a_version_one_ledger_folds_and_a_malformed_one_fails() {
    let mut ledger = Ledger::new("v1");
    let v1 = json!({
        "schema_version": 1,
        "scope": ["other.py"],
        "last_sweep_scope": ["other.py"],
        "items": [{
            "id": "0000000000000000", "module": "other.py", "qualname": "old", "verdict": "NOT_EXERCISED",
            "tier": "shipped", "rules": ["drop_clamp"], "findings": 1, "line": 9, "description": "x",
        }],
    });
    std::fs::write(ledger.ledger(), v1.to_string()).unwrap();
    first_sweep(&mut ledger);
    assert!(!ledger
        .items()
        .contains(&item("other.py", "old", "NOT_EXERCISED")));
    std::fs::write(ledger.ledger(), "{not json").unwrap();
    let path = ledger.tree.path().join("run-1.json");
    let paths = [path.to_str().unwrap().to_owned()];
    assert!(backlog(&paths, &ledger.ledger(), &ledger.report(), false).is_err());
}
