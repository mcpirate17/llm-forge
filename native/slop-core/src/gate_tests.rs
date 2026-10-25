//! The gate's contract: every target is measured or says why not, a finding is filed
//! by what it means, and the report and check read the same summary.

use crate::gate::{gate, Measured, Summary, Target};
use crate::gate_report::{check_findings, findings, metrics, render, sci, write_artifact};
use crate::index_tests::Tree;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

/// A stand-in interpreter: `<module>.report` is the report it writes, and a few
/// module names misbehave the ways a real child can.
const FAKE_PYTHON: &str = r#"#!/bin/sh
module=$3
report=
prev=
for arg in "$@"; do
  [ "$prev" = "--json" ] && report=$arg
  prev=$arg
done
echo "$@" > "$module.args"
case "$module" in
  slow.py) exec sleep 30 ;;
  crash.py) echo "boom" >&2; exit 3 ;;
  silent.py) exit 0 ;;
  garbled.py) echo "not json" > "$report" ;;
  *) cp "$module.report" "$report" ;;
esac
"#;

fn tree(tag: &str) -> (Tree, String) {
    let tree = Tree::new(&format!("gate-{tag}"));
    tree.write("fake-python", FAKE_PYTHON);
    let python = tree.path().join("fake-python");
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let python = python.to_str().unwrap().to_owned();
    (tree, python)
}

fn finding(qualname: &str, verdict: &str, line: usize) -> Value {
    json!({
        "qualname": qualname,
        "rule": "drop_clamp",
        "description": "clamp removed",
        "lineno": line,
        "verdict": verdict,
        "calls_seen": 3,
        "amplifier": "params_x50",
        "max_diff_amplified": 0.000012345,
    })
}

fn report(tree: &Tree, module: &str, findings: &[Value]) {
    tree.write(
        &format!("{module}.report"),
        &Value::from(findings.to_vec()).to_string(),
    );
}

fn unit(qualname: &str, line: usize, tests: &[&str]) -> (String, usize, Vec<String>) {
    let tests = tests.iter().map(|test| test.to_string()).collect();
    (qualname.to_owned(), line, tests)
}

fn run(tree: &Tree, python: &str, targets: &[Target]) -> Summary {
    gate(
        tree.path(),
        python,
        targets,
        None,
        Duration::from_secs(5),
        None,
    )
    .unwrap()
}

fn verdicts(findings: &[Value]) -> Vec<(&str, &str)> {
    findings
        .iter()
        .map(|f| {
            (
                f["qualname"].as_str().unwrap(),
                f["verdict"].as_str().unwrap(),
            )
        })
        .collect()
}

fn leftovers(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".equivalence-gate-"))
        .collect()
}

#[test]
fn a_module_is_probed_with_its_units_drivers_and_only_its_units() {
    let (tree, python) = tree("record");
    let reachable = finding("f", "REACHABLE_BUT_UNTESTED", 3);
    report(
        &tree,
        "good.py",
        &[reachable, finding("C.m", "NO_DIFFERENCE_OBSERVED", 8)],
    );
    let units = vec![
        unit("f", 2, &["t.py::a"]),
        unit("C.m", 7, &["t.py::b", "t.py::a"]),
    ];
    let summary = run(&tree, &python, &[("good.py".into(), false, units, vec![])]);

    let args = std::fs::read_to_string(tree.path().join("good.py.args")).unwrap();
    assert!(
        args.contains("good.py t.py::a t.py::b --function f --function C.m --json"),
        "{args}"
    );
    assert_eq!(
        verdicts(&summary.blocking),
        [("f", "REACHABLE_BUT_UNTESTED")]
    );
    assert_eq!(
        verdicts(&summary.advisory),
        [("C.m", "NO_DIFFERENCE_OBSERVED")]
    );
    assert_eq!(summary.blocking[0]["module"], "good.py");
    assert_eq!(summary.blocking[0]["drivers_source"], "record");
    assert_eq!(summary.modules_probed, 1);
    let units = Some(vec!["f".to_owned(), "C.m".to_owned()]);
    assert_eq!(
        summary.measured,
        [Measured {
            module: "good.py".into(),
            units
        }]
    );
    assert!(leftovers(tree.path()).is_empty());
}

#[test]
fn a_unit_no_recorded_test_ran_is_a_hole_in_the_record() {
    // Its module's other units bring the drivers, so it is probed, but nothing recorded
    // ran its lines: reporting it as a selection miss would hide a coverage hole.
    let (tree, python) = tree("unrecorded");
    report(
        &tree,
        "good.py",
        &[
            finding("f", "NOT_EXERCISED", 3),
            finding("g", "NOT_EXERCISED", 9),
        ],
    );
    let units = vec![unit("f", 2, &["t.py::a"]), unit("g", 8, &[])];
    let summary = run(&tree, &python, &[("good.py".into(), false, units, vec![])]);
    let sources: Vec<&str> = summary
        .untested
        .iter()
        .map(|f| f["drivers_source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["record", "unrecorded"]);
    assert_eq!(metrics(&summary)["untested_unrecorded"], 1);
}

#[test]
fn a_unit_refused_for_its_drivers_time_is_incomplete_and_the_rest_is_probed() {
    let (tree, python) = tree("refused");
    report(&tree, "good.py", &[finding("f", "LIVE", 3)]);
    let refused = |qualname: &str| (qualname.to_owned(), 8, "its driver takes 140.0s".to_owned());
    let targets: Vec<Target> = vec![
        (
            "good.py".into(),
            false,
            vec![unit("f", 2, &["t.py::a"])],
            vec![refused("g")],
        ),
        // Its only recorded driver was refused, so the index must not stand in for it.
        (
            "refused.py".into(),
            false,
            vec![unit("h", 1, &[])],
            vec![refused("k")],
        ),
    ];
    let summary = run(&tree, &python, &targets);
    assert_eq!(summary.live, 1);
    assert_eq!(
        verdicts(&summary.incomplete),
        [("g", "OVER_BUDGET"), ("k", "OVER_BUDGET")]
    );
    assert_eq!(summary.modules_without_drivers, ["refused.py"]);
    assert!(summary.modules_index_driven.is_empty());
    let check = check_findings(&summary).unwrap();
    assert_eq!(check[0]["rule_id"], "probe-over_budget");
    assert_eq!(check[0]["severity"], "medium");
    assert!(render(&summary)
        .unwrap()
        .contains("INCOMPLETE [over_budget] good.py::g  its driver takes 140.0s\n"));
}

#[test]
fn a_whole_module_is_measured_whole() {
    let (tree, python) = tree("whole");
    report(&tree, "good.py", &[finding("f", "LIVE", 3)]);
    let summary = run(
        &tree,
        &python,
        &[(
            "good.py".into(),
            true,
            vec![unit("f", 2, &["t.py::a"])],
            vec![],
        )],
    );
    assert_eq!(
        summary.measured,
        [Measured {
            module: "good.py".into(),
            units: None
        }]
    );
    assert_eq!(summary.live, 1);
}

#[test]
fn a_child_that_times_out_fails_or_writes_no_report_is_incomplete_not_clean() {
    let (tree, python) = tree("broken");
    let targets: Vec<Target> = ["slow.py", "crash.py", "silent.py", "garbled.py"]
        .into_iter()
        .map(|module| {
            (
                module.to_owned(),
                false,
                vec![unit("f", 1, &["t.py::a"])],
                vec![],
            )
        })
        .collect();
    let summary = gate(
        tree.path(),
        &python,
        &targets,
        None,
        Duration::from_secs(1),
        Some(4),
    )
    .unwrap();

    let rules: Vec<(&str, &str, &str)> = summary
        .incomplete
        .iter()
        .map(|f| {
            let field = |key: &str| f[key].as_str().unwrap();
            (field("module"), field("verdict"), field("rule"))
        })
        .collect();
    assert_eq!(
        rules,
        [
            ("slow.py", "TIMEOUT", "timeout"),
            ("crash.py", "PROBE_FAILED", "probe-exit"),
            ("silent.py", "PROBE_FAILED", "probe-no-report"),
            ("garbled.py", "PROBE_FAILED", "probe-bad-report"),
        ]
    );
    assert!(summary.incomplete[1]["description"]
        .as_str()
        .unwrap()
        .contains("exited 3"));
    assert_eq!(summary.incomplete[1]["stderr_tail"], "boom\n");
    assert_eq!(summary.modules_probed, 0);
    assert!(summary.measured.is_empty());
    assert!(leftovers(tree.path()).is_empty());
}

#[test]
fn a_unit_with_ablations_the_report_never_mentions_is_unreported() {
    let (tree, python) = tree("unreported");
    tree.write(
        "part.py",
        "def f(x):\n    return max(x, 0)\n\n\ndef g(x):\n    return min(x, 0)\n",
    );
    report(
        &tree,
        "part.py",
        &[finding("f", "NO_DIFFERENCE_OBSERVED", 2)],
    );
    let units = vec![unit("f", 1, &["t.py::a"]), unit("g", 5, &["t.py::a"])];
    let summary = run(&tree, &python, &[("part.py".into(), false, units, vec![])]);
    assert_eq!(verdicts(&summary.incomplete), [("g", "UNREPORTED")]);
    assert_eq!(summary.incomplete[0]["lineno"], 5);
    assert_eq!(summary.incomplete[0]["rule"], "probe-unit-unreported");
    assert_eq!(summary.modules_probed, 1);
}

#[test]
fn a_module_no_recorded_test_ran_is_driven_by_the_test_files_importing_it() {
    let (tree, python) = tree("index");
    tree.write("pkg/__init__.py", "");
    tree.write(
        "pkg/mod.py",
        "def f():\n    return 1\n\n\ndef g():\n    return 2\n",
    );
    tree.write(
        "tests/test_mod.py",
        "from pkg import mod\n\n\ndef test_g():\n    assert mod.g()\n",
    );
    tree.write("tests/test_other.py", "def test_nothing():\n    pass\n");
    report(
        &tree,
        "pkg/mod.py",
        &[
            finding("f", "NOT_EXERCISED", 2),
            finding("g", "NOT_EXERCISED", 5),
        ],
    );
    let units = vec![unit("f", 1, &[]), unit("g", 4, &[])];
    let targets: Vec<Target> = vec![
        ("pkg/mod.py".into(), true, units, vec![]),
        ("pkg/none.py".into(), true, vec![], vec![]),
    ];
    let summary = run(&tree, &python, &targets);

    let args = std::fs::read_to_string(tree.path().join("pkg/mod.py.args")).unwrap();
    assert!(
        args.contains("pkg/mod.py tests/test_mod.py --function f"),
        "{args}"
    );
    assert_eq!(summary.modules_index_driven, ["pkg/mod.py"]);
    assert_eq!(verdicts(&summary.untested), [("f", "NOT_EXERCISED")]);
    assert_eq!(
        verdicts(&summary.unreached),
        [("g", "NOT_REACHED_BY_DRIVERS")]
    );
    assert_eq!(
        summary.unreached[0]["named_by"],
        json!(["tests/test_mod.py"])
    );
    assert_eq!(summary.unreached[0]["drivers_source"], "index");
}

#[test]
fn a_module_nothing_drives_is_reported_without_drivers() {
    let (tree, python) = tree("undriven");
    let summary = run(
        &tree,
        &python,
        &[("lonely.py".into(), true, vec![unit("f", 1, &[])], vec![])],
    );
    assert_eq!(summary.modules_without_drivers, ["lonely.py"]);
    assert_eq!(summary.modules_probed, 0);
}

#[test]
fn a_waiver_moves_a_blocking_finding_to_advisory_and_says_so() {
    let (tree, python) = tree("waived");
    report(
        &tree,
        "good.py",
        &[finding("f", "REACHABLE_BUT_UNTESTED", 3)],
    );
    tree.write(
        "waivers.json",
        r#"{"waivers": [{"module": "good.py", "rule": "drop_clamp", "reason": "measured"}]}"#,
    );
    let targets: Vec<Target> = vec![(
        "good.py".into(),
        true,
        vec![unit("f", 2, &["t.py::a"])],
        vec![],
    )];
    let waivers = tree.path().join("waivers.json");
    let summary = gate(
        tree.path(),
        &python,
        &targets,
        Some(&waivers),
        Duration::from_secs(5),
        None,
    )
    .unwrap();
    assert!(summary.blocking.is_empty());
    assert_eq!(summary.advisory[0]["waived"], true);
    let missing = tree.path().join("absent.json");
    assert!(gate(
        tree.path(),
        &python,
        &targets,
        Some(&missing),
        Duration::from_secs(5),
        None
    )
    .is_err());
}

#[test]
fn an_unknown_verdict_fails_the_run() {
    let (tree, python) = tree("unknown");
    report(&tree, "good.py", &[finding("f", "SOMETHING_NEW", 3)]);
    let targets: Vec<Target> = vec![(
        "good.py".into(),
        true,
        vec![unit("f", 2, &["t.py::a"])],
        vec![],
    )];
    let error = gate(
        tree.path(),
        &python,
        &targets,
        None,
        Duration::from_secs(5),
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("SOMETHING_NEW"), "{error:#}");
}

#[test]
fn zero_jobs_is_an_error_not_a_hang() {
    let (tree, python) = tree("zero");
    let targets: Vec<Target> = vec![(
        "good.py".into(),
        true,
        vec![unit("f", 2, &["t.py::a"])],
        vec![],
    )];
    assert!(gate(
        tree.path(),
        &python,
        &targets,
        None,
        Duration::from_secs(5),
        Some(0)
    )
    .is_err());
}

#[test]
fn sci_writes_what_python_writes() {
    assert_eq!(sci(0.000012345), "1.234e-05");
    assert_eq!(sci(12345.0), "1.234e+04");
    assert_eq!(sci(0.0), "0.000e+00");
    assert_eq!(sci(1e-123), "1.000e-123");
    assert_eq!(sci(f64::INFINITY), "inf");
}

fn sample() -> Summary {
    let row = |qualname: &str, verdict: &str, source: &str| {
        let mut f = finding(qualname, verdict, 4);
        f["module"] = json!("m.py");
        f["drivers_source"] = json!(source);
        f
    };
    let mut timeout = row("m.py", "TIMEOUT", "record");
    timeout["rule"] = json!("timeout");
    timeout["duration_s"] = json!(180.0);
    timeout["stderr_tail"] = json!("one\ntwo\n");
    Summary {
        modules_probed: 1,
        blocking: vec![row("f", "REACHABLE_BUT_UNTESTED", "record")],
        advisory: vec![row("g", "NO_DIFFERENCE_OBSERVED", "record")],
        untested: vec![
            row("h", "NOT_EXERCISED", "index"),
            row("k", "NOT_EXERCISED", "record"),
            row("n", "NOT_EXERCISED", "unrecorded"),
        ],
        unreached: vec![
            row("u", "NOT_REACHED_BY_DRIVERS", "index"),
            row("u", "NOT_REACHED_BY_DRIVERS", "index"),
        ],
        unmeasured: vec![
            row("o", "OVER_BUDGET", "record"),
            row("o", "OVER_BUDGET", "record"),
        ],
        incomplete: vec![timeout],
        live: 2,
        ..Summary::default()
    }
}

#[test]
fn the_report_lists_each_bucket_and_every_count() {
    let text = render(&sample()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "BLOCKING  m.py::f:4  clamp removed");
    assert!(lines[1].contains("reachable via params_x50 (relative change 1.234e-05)"));
    assert_eq!(
        lines[2],
        "advisory  [no_difference_observed] m.py::g:4  clamp removed"
    );
    assert!(text.contains("1 function(s) no test file anywhere names"));
    assert!(text.contains("1 function(s) whose recorded tests ran their lines"));
    assert!(text.contains("1 function(s) no recorded passing test ran"));
    assert!(text.contains(
        "INCOMPLETE [timeout] m.py  clamp removed (180.0s)\n            one\n            two\n"
    ));
    assert!(text.contains("1 function(s) a test file names but"));
    assert!(text.contains("2 construct(s) in 1 function(s) were never measured"));
    assert!(text.ends_with(
        "blocking=1 advisory=1 live=2 probed=1 incomplete=1 unmeasured=2 unreached=2 untested=3 \
         index_driven=0 without_drivers=0\n"
    ));
}

#[test]
fn the_check_blocks_on_reachable_and_reports_what_went_unmeasured() {
    let summary = sample();
    let findings = check_findings(&summary).unwrap();
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["severity"], "high");
    assert_eq!(findings[0]["rule_id"], "drop_clamp");
    assert!(findings[0]["message"]
        .as_str()
        .unwrap()
        .contains("(relative change 1.234e-05)"));
    assert_eq!(findings[1]["severity"], "medium");
    assert_eq!(findings[1]["rule_id"], "probe-timeout");
    let counts = metrics(&summary);
    assert_eq!(counts["timed_out"], 1);
    assert_eq!(counts["untested_index"], 1);
    assert_eq!(counts["untested_record"], 1);
    assert_eq!(counts["untested_unrecorded"], 1);
    assert_eq!(counts["live"], 2);
}

#[test]
fn a_blocking_finding_without_its_measurement_fails_loud() {
    let mut summary = sample();
    summary.blocking[0]
        .as_object_mut()
        .unwrap()
        .remove("max_diff_amplified");
    assert!(check_findings(&summary).is_err());
    assert!(render(&summary).is_err());
}

#[test]
fn an_incomparable_difference_is_measured_and_reported_as_inf() {
    // The probe names a difference it cannot compare "inf": JSON has no infinity.
    let (tree, python) = tree("inf");
    let mut reachable = finding("f", "REACHABLE_BUT_UNTESTED", 3);
    reachable["max_diff_amplified"] = json!("inf");
    reachable["max_diff_recorded"] = json!("nan");
    report(&tree, "good.py", &[reachable]);
    let units = vec![unit("f", 2, &["t.py::a"])];
    let mut summary = run(&tree, &python, &[("good.py".into(), true, units, vec![])]);
    assert!(summary.incomplete.is_empty(), "{:?}", summary.incomplete);
    let check = check_findings(&summary).unwrap();
    assert_eq!(check[0]["severity"], "high");
    assert!(check[0]["message"]
        .as_str()
        .unwrap()
        .contains("(relative change inf)"));
    summary.blocking[0]["max_diff_amplified"] = json!("large");
    assert!(check_findings(&summary).is_err());
}

#[test]
fn a_run_artifact_is_renamed_into_place_whole() {
    let tree = Tree::new("gate-artifact");
    let path = write_artifact(&tree.path().join("runs"), "{}").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("gate-") && name.ends_with(".json"),
        "{name}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    assert_eq!(
        std::fs::read_dir(tree.path().join("runs")).unwrap().count(),
        1
    );
}

#[test]
fn an_unwritable_run_artifact_is_a_finding_not_a_failed_review() {
    let tree = Tree::new("gate-artifact-unwritable");
    tree.write("runs", "a file where the directory should be");
    let blocker = tree.path().join("runs");
    let summary = serde_json::to_string(&Summary::default()).unwrap();
    let (rows, metrics) = findings(&summary, Some(&blocker.join("gate"))).unwrap();
    let rows: Vec<Value> = serde_json::from_str(&rows).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["rule_id"], "run-artifact-unwritten");
    assert_eq!(rows[0]["severity"], "medium");
    let metrics: Value = serde_json::from_str(&metrics).unwrap();
    assert_eq!(metrics["modules_probed"], 0);
}
