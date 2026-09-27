#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped slop backlog and its native aggregation.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

fn ledger<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.slop_ledger")
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let serialized: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&serialized).unwrap()
}

fn finding(module: &str, name: &str, verdict: &str, rule: &str, line: i32) -> Value {
    json!({
        "module": module, "qualname": name, "verdict": verdict, "rule": rule,
        "lineno": line, "description": format!("{rule} removed"),
    })
}

fn simple(module: &str, name: &str, verdict: &str) -> Value {
    finding(module, name, verdict, "drop_raise_guard", 10)
}

fn aggregate(py: Python<'_>, findings: Vec<Value>) -> Value {
    json_value(
        py,
        &ledger(py)
            .getattr("aggregate")
            .unwrap()
            .call1((py_json(py, json!(findings)),))
            .unwrap(),
    )
}

fn save(py: Python<'_>, file: &Path, items: Value, scope: &[&str]) {
    ledger(py)
        .getattr("save")
        .unwrap()
        .call1((py_json(py, items), scope, path(py, file)))
        .unwrap();
}

fn load(py: Python<'_>, file: &Path) -> Value {
    json_value(
        py,
        &ledger(py)
            .getattr("load")
            .unwrap()
            .call1((path(py, file),))
            .unwrap(),
    )
}

fn diff(py: Python<'_>, previous: Value, items: Value, scope: &[&str]) -> Value {
    json_value(
        py,
        &ledger(py)
            .getattr("diff")
            .unwrap()
            .call1((py_json(py, previous), py_json(py, items), scope))
            .unwrap(),
    )
}

fn render(py: Python<'_>, new: Value, carried: Value, fixed: Value, scope: &[&str]) -> String {
    ledger(py)
        .getattr("render")
        .unwrap()
        .call1((
            py_json(py, new),
            py_json(py, carried),
            py_json(py, fixed),
            scope,
        ))
        .unwrap()
        .extract()
        .unwrap()
}

fn capture_stdout<'py>(py: Python<'py>) -> (Bound<'py, PyAny>, AttrPatch) {
    let output = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(module(py, "sys").as_any(), "stdout", &output);
    (output, patch)
}

#[test]
fn findings_collapse_to_one_item_per_function_and_verdict() {
    let _case = Case::new();
    Python::attach(|py| {
        let items = aggregate(
            py,
            vec![
                finding("conductor/a.py", "run", "NOT_EXERCISED", "drop_clamp", 40),
                finding("conductor/a.py", "run", "NOT_EXERCISED", "drop_detach", 12),
                finding("conductor/a.py", "run", "NOT_EXERCISED", "drop_where", 30),
            ],
        );
        let rows = items.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["findings"], 3);
        assert_eq!(
            rows[0]["rules"],
            json!(["drop_clamp", "drop_detach", "drop_where"])
        );
        assert_eq!(rows[0]["line"], 12);
    });
}

#[test]
fn shipped_code_outranks_a_much_larger_pile_of_exploratory() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut findings = vec![simple("conductor/a.py", "run", "NOT_EXERCISED")];
        findings.extend((0..30).map(|i| {
            finding(
                "research/tools/x.py",
                "helper",
                "NOT_EXERCISED",
                &format!("r{i}"),
                10,
            )
        }));
        let items = aggregate(py, findings);
        assert_eq!(items[0]["tier"], "shipped");
        assert_eq!(items[0]["module"], "conductor/a.py");
        assert_eq!(items[1]["tier"], "exploratory");
        assert_eq!(items[1]["findings"], 30);
    });
}

#[test]
fn research_tools_is_not_shipped_but_research_synthesis_is() {
    let _case = Case::new();
    Python::attach(|py| {
        let items = aggregate(
            py,
            vec![
                simple("research/tools/x.py", "a", "NOT_EXERCISED"),
                simple("research/synthesis/y.py", "b", "NOT_EXERCISED"),
            ],
        );
        let tiers = items
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["module"].as_str().unwrap(),
                    row["tier"].as_str().unwrap(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(tiers["research/tools/x.py"], "exploratory");
        assert_eq!(tiers["research/synthesis/y.py"], "shipped");
    });
}

#[test]
fn rerun_of_same_sweep_reports_nothing_new() {
    let case = Case::new();
    let file = case.root().join("ledger.json");
    Python::attach(|py| {
        let items = aggregate(py, vec![simple("conductor/a.py", "run", "NOT_EXERCISED")]);
        save(py, &file, items.clone(), &["conductor/a.py"]);
        let result = diff(py, load(py, &file), items, &["conductor/a.py"]);
        assert_eq!(result[0], json!([]));
        assert_eq!(result[1].as_array().unwrap().len(), 1);
        assert_eq!(result[2], json!([]));
    });
}

#[test]
fn fixed_counts_only_modules_this_sweep_actually_covered() {
    let case = Case::new();
    let file = case.root().join("ledger.json");
    Python::attach(|py| {
        let both = aggregate(
            py,
            vec![
                simple("conductor/a.py", "run", "NOT_EXERCISED"),
                simple("conductor/b.py", "run", "NOT_EXERCISED"),
            ],
        );
        save(py, &file, both, &["conductor/a.py", "conductor/b.py"]);
        let result = diff(py, load(py, &file), json!([]), &["conductor/a.py"]);
        assert_eq!(
            result[2]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["module"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["conductor/a.py"]
        );
        assert_eq!(result[0], json!([]));
        assert_eq!(result[1], json!([]));
    });
}

#[test]
fn narrow_sweep_does_not_erase_rest_of_backlog() {
    let case = Case::new();
    let file = case.root().join("ledger.json");
    Python::attach(|py| {
        save(
            py,
            &file,
            aggregate(
                py,
                vec![
                    simple("conductor/a.py", "run", "NOT_EXERCISED"),
                    simple("conductor/b.py", "run", "NOT_EXERCISED"),
                ],
            ),
            &["conductor/a.py", "conductor/b.py"],
        );
        save(
            py,
            &file,
            aggregate(
                py,
                vec![simple("conductor/a.py", "run", "NO_DIFFERENCE_OBSERVED")],
            ),
            &["conductor/a.py"],
        );
        let saved = load(py, &file);
        let kept = saved["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["module"].as_str().unwrap(),
                    row["verdict"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(kept.contains(&("conductor/b.py", "NOT_EXERCISED")));
        assert!(kept.contains(&("conductor/a.py", "NO_DIFFERENCE_OBSERVED")));
    });
}

#[test]
fn item_survives_file_being_edited_around_it() {
    let _case = Case::new();
    Python::attach(|py| {
        let first = aggregate(py, vec![simple("conductor/a.py", "run", "NOT_EXERCISED")]);
        let second = aggregate(
            py,
            vec![finding(
                "conductor/a.py",
                "run",
                "NOT_EXERCISED",
                "drop_raise_guard",
                900,
            )],
        );
        assert_eq!(first[0]["id"], second[0]["id"]);
    });
}

#[test]
fn report_lists_shipped_items_and_only_counts_exploratory() {
    let _case = Case::new();
    Python::attach(|py| {
        let items = aggregate(
            py,
            vec![
                simple("conductor/a.py", "shipped_fn", "NOT_EXERCISED"),
                simple("research/tools/x.py", "throwaway_fn", "NOT_EXERCISED"),
            ],
        );
        let report = render(
            py,
            items,
            json!([]),
            json!([]),
            &["conductor/a.py", "research/tools/x.py"],
        );
        assert!(report.contains("shipped_fn"));
        assert!(!report.contains("throwaway_fn"));
        assert!(report.contains("Exploratory (1 new)"));
    });
}

#[test]
fn report_says_so_when_nothing_changed() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(
            render(py, json!([]), json!([]), json!([]), &["conductor/a.py"])
                .contains("Nothing new and nothing fixed")
        )
    });
}

#[test]
fn cli_writes_nothing_on_dry_run() {
    let case = Case::new();
    let sweep = case.write("sweep.json", &json!({
        "blocking":[],"advisory":[simple("conductor/a.py","run","NO_DIFFERENCE_OBSERVED")],"untested":[],"modules_without_drivers":[]
    }).to_string());
    let file = case.root().join("ledger.json");
    let report = case.root().join("report.md");
    Python::attach(|py| {
        let (stdout, _patch) = capture_stdout(py);
        let code: i32 = ledger(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                sweep.to_str().unwrap(),
                "--ledger",
                file.to_str().unwrap(),
                "--report",
                report.to_str().unwrap(),
                "--dry-run",
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let output: String = stdout.call_method0("getvalue").unwrap().extract().unwrap();
        assert!(output.contains("Slop backlog"));
        assert!(!file.exists());
        assert!(!report.exists());
    });
}

#[test]
fn cli_writes_both_artifacts() {
    let case = Case::new();
    let sweep = case.write("sweep.json", &json!({
        "blocking":[],"advisory":[],"untested":[simple("conductor/a.py","run","NOT_EXERCISED")],"modules_without_drivers":[]
    }).to_string());
    let file = case.root().join("ledger.json");
    let report = case.root().join("report.md");
    Python::attach(|py| {
        let code: i32 = ledger(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                sweep.to_str().unwrap(),
                "--ledger",
                file.to_str().unwrap(),
                "--report",
                report.to_str().unwrap(),
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        assert!(fs::read_to_string(&report).unwrap().contains("run"));
        let contents: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(contents["items"][0]["qualname"], "run");
    });
}

#[test]
fn every_bucket_of_summary_reaches_backlog() {
    let _case = Case::new();
    Python::attach(|py| {
        let summary = json!({
            "blocking":[simple("conductor/a.py","b","REACHABLE_BUT_UNTESTED")],
            "advisory":[simple("conductor/a.py","c","NO_DIFFERENCE_OBSERVED")],
            "untested":[simple("conductor/a.py","d","NOT_EXERCISED")],
        });
        let findings = ledger(py)
            .getattr("findings_from_summary")
            .unwrap()
            .call1((py_json(py, summary),))
            .unwrap();
        assert_eq!(findings.len().unwrap(), 3);
    });
}

#[test]
fn missing_ledger_is_first_run_not_crash() {
    let case = Case::new();
    Python::attach(|py| {
        assert_eq!(load(py, &case.root().join("nope.json"))["items"], json!([]));
        let stub = case.write("stub.json", "");
        assert_eq!(load(py, &stub)["items"], json!([]));
    });
}

#[test]
fn ledger_scope_covers_every_module_it_speaks_for() {
    let case = Case::new();
    let file = case.root().join("ledger.json");
    Python::attach(|py| {
        save(
            py,
            &file,
            aggregate(
                py,
                vec![
                    simple("conductor/a.py", "run", "NOT_EXERCISED"),
                    simple("conductor/b.py", "run", "NOT_EXERCISED"),
                ],
            ),
            &["conductor/a.py", "conductor/b.py"],
        );
        save(
            py,
            &file,
            aggregate(py, vec![simple("conductor/a.py", "run", "NOT_EXERCISED")]),
            &["conductor/a.py"],
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(
            written["scope"],
            json!(["conductor/a.py", "conductor/b.py"])
        );
        assert_eq!(written["last_sweep_scope"], json!(["conductor/a.py"]));
    });
}

#[test]
fn sweep_and_gate_run_fold_into_one_backlog() {
    let _case = Case::new();
    Python::attach(|py| {
        let summaries = json!([
            {"blocking":[{"module":"a.py"}],"untested":[{"module":"c.py"}],"modules_without_drivers":["x.py"],"modules_probed":2},
            {"advisory":[{"module":"b.py"}],"modules_without_drivers":["x.py","y.py"],"modules_probed":3},
        ]);
        let merged = json_value(
            py,
            &ledger(py)
                .getattr("merge_summaries")
                .unwrap()
                .call1((py_json(py, summaries),))
                .unwrap(),
        );
        assert_eq!(merged["blocking"], json!([{"module":"a.py"}]));
        assert_eq!(merged["advisory"], json!([{"module":"b.py"}]));
        assert_eq!(merged["untested"], json!([{"module":"c.py"}]));
        assert_eq!(merged["modules_probed"], 5);
        assert_eq!(merged["modules_without_drivers"], json!(["x.py", "y.py"]));
    });
}

#[test]
fn cli_folds_every_summary_it_is_given() {
    let case = Case::new();
    let sweep = case.write("sweep.json", &json!({"modules_probed":1,"modules_without_drivers":[],"advisory":[],"blocking":[simple("conductor/swept.py","swept","REACHABLE_BUT_UNTESTED")],"untested":[]}).to_string());
    let gate = case.write("gate.json", &json!({"modules_probed":1,"modules_without_drivers":[],"advisory":[],"blocking":[simple("conductor/from_gate.py","gated","REACHABLE_BUT_UNTESTED")],"untested":[]}).to_string());
    let file = case.root().join("ledger.json");
    let report = case.root().join("report.md");
    Python::attach(|py| {
        let code: i32 = ledger(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                sweep.to_str().unwrap(),
                gate.to_str().unwrap(),
                "--ledger",
                file.to_str().unwrap(),
                "--report",
                report.to_str().unwrap(),
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let saved: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        let modules = saved["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["module"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            modules,
            ["conductor/swept.py", "conductor/from_gate.py"]
                .into_iter()
                .collect()
        );
    });
}
