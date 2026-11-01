#![cfg(feature = "python-compat-tests")]
//! Cargo, CTest, command capture, and campaign routing contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/mutation_value_support.rs"]
#[allow(dead_code)]
mod value_support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};
use value_support::{from_python, kwargs, to_python};

type RunnerCalls = Arc<Mutex<Vec<(Vec<String>, bool)>>>;

const ALPHA: &str = "tooling/native/conductor-native/src/mutation_receipt.rs::test_alpha";
const BETA: &str = "tooling/native/conductor-native/src/mutation_manifest.rs::test_beta";
const CTEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="(empty)" tests="4" failures="1" disabled="1" skipped="0">
  <testcase name="test_profiler.test_memory_events" classname="c" time="0.03" status="run"/>
  <testcase name="test_profiler.test_reset_clears_all" classname="c" time="0.02" status="fail"><failure message="Failed"/></testcase>
  <testcase name="test_profiler.test_clock_ns_monotonic" classname="c" time="0" status="disabled"/>
  <testcase name="test_kernels.test_relu" classname="c" time="0.01" status="fail"><failure message="Failed"/></testcase>
</testsuite>"#;

fn namespace<'py>(py: Python<'py>, fields: &[(&str, Value)]) -> Bound<'py, PyAny> {
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs(py, fields)))
        .unwrap()
}

fn verdict(py: Python<'_>, report: &Bound<'_, PyAny>, expected: &[&str]) -> Value {
    let mutation = namespace(py, &[("expected_killers", json!(expected))]);
    from_python(
        &module(py, "conductor.mutation_testing")
            .getattr("killer_verdict")
            .unwrap()
            .call1((mutation, report, "KILLED"))
            .unwrap(),
    )
}

#[test]
fn cargo_raw_output_requires_target_provenance() {
    let _case = Case::new();
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let supports = value.getattr("cargo_attribution_supported").unwrap();
        assert!(supports
            .call1((vec![ALPHA, BETA],))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let result = from_python(
            &value
                .getattr("parse_cargo_libtest")
                .unwrap()
                .call1(("test receipt::tests::test_alpha ... FAILED\n", vec![ALPHA]))
                .unwrap(),
        );
        assert_eq!(result["status"], "INCOMPLETE");
        assert_eq!(result["tests"], json!({}));
        assert!(result["error"].as_str().unwrap().contains("provenance"));
    });
}

#[test]
fn cargo_collector_binds_real_single_target_and_rejects_other_target() {
    let case = Case::new();
    let root = case.mkdir("cargo-bound");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "#[test] fn shared() {}\n").unwrap();
    std::fs::write(root.join("tests/other.rs"), "#[test] fn shared() {}\n").unwrap();
    let run = |target: &[&str]| {
        let output = std::process::Command::new("cargo")
            .args(target)
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let lib_argv = [
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        "fixture",
        "--lib",
    ];
    let other_argv = [
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        "fixture",
        "--test",
        "other",
    ];
    let lib_output = run(&lib_argv[1..]);
    let other_output = run(&other_argv[1..]);
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let collect = value.getattr("collect_cargo_libtest_batch").unwrap();
        let check = |argv: &[&str], stdout: &str| {
            let text = stdout.to_owned();
            let sink = PyCFunction::new_closure(
                py,
                None,
                None,
                move |args: &Bound<'_, PyTuple>, _| -> PyResult<&'static str> {
                    args.get_item(1)?.call1((text.as_str(),))?;
                    Ok("executed")
                },
            )
            .unwrap();
            let options = PyDict::new(py);
            options.set_item("argv", argv).unwrap();
            options
                .set_item("ranked_nodeids", vec!["src/lib.rs::shared"])
                .unwrap();
            options.set_item("snapshot_root", path(py, &root)).unwrap();
            options.set_item("run_command", sink).unwrap();
            from_python(
                &collect
                    .call((), Some(&options))
                    .unwrap()
                    .get_item(1)
                    .unwrap(),
            )
        };
        let good = check(&lib_argv, &lib_output);
        assert_eq!(good["status"], "COMPLETE");
        assert_eq!(good["tests"]["src/lib.rs::shared"]["outcome"], "PASSED");
        assert!(other_output.contains("test shared ... ok"));
        assert_eq!(check(&other_argv, &other_output)["status"], "INCOMPLETE");
    });
}

#[test]
fn cargo_verdict_separates_declared_and_collateral_kills() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = to_python(
            py,
            &json!({"status":"COMPLETE", "tests":{
            (ALPHA):{"outcome":"PASSED"}, (BETA):{"outcome":"FAILED"}},
            "unranked_failures":["other::test_blunt"]}),
        );
        let result = verdict(py, &report, &[ALPHA]);
        assert_eq!(result["status"], "MISATTRIBUTED");
        assert_eq!(result["observed_failures"], json!([BETA]));
        assert_eq!(result["collateral"], json!([BETA]));
        let declared = to_python(
            py,
            &json!({"status":"COMPLETE", "tests":{
            (ALPHA):{"outcome":"FAILED"}, (BETA):{"outcome":"PASSED"}}, "unranked_failures":[]}),
        );
        assert_eq!(verdict(py, &declared, &[ALPHA])["status"], "CONFIRMED");
        let incomplete = to_python(
            py,
            &json!({"status":"INCOMPLETE", "tests":{}, "missing_nodeids":[ALPHA]}),
        );
        assert_eq!(verdict(py, &incomplete, &[ALPHA])["status"], "UNATTRIBUTED");
    });
}

#[test]
fn capable_unattributed_kills_refuse_campaign() {
    let _case = Case::new();
    Python::attach(|py| {
        let enforce = module(py, "conductor.mutation_testing")
            .getattr("killer_enforcement")
            .unwrap();
        let run = |rows: Value| from_python(&enforce.call1((to_python(py, &rows),)).unwrap());
        let row =
            |id: &str, status: &str| json!({"id": id, "killer_attribution": {"status": status}});
        assert_eq!(
            run(json!([row("a", "CONFIRMED"), row("b", "CONFIRMED")])),
            json!({
            "status": "ENFORCED", "misattributed": [], "unattributed": [], "unattributed_runs": []})
        );
        let unavailable = run(json!([row("a", "CONFIRMED"), row("b", "UNAVAILABLE")]));
        assert_eq!(unavailable["status"], "UNAVAILABLE");
        assert_eq!(unavailable["unattributed"], json!(["b"]));
        let unattributed = run(json!([row("a", "CONFIRMED"), row("b", "UNATTRIBUTED")]));
        assert_eq!(unattributed["status"], "REFUSED");
        assert_eq!(unattributed["unattributed_runs"], json!(["b"]));
        let wrong = run(json!([row("a", "MISATTRIBUTED"), row("b", "UNAVAILABLE")]));
        assert_eq!(wrong["status"], "REFUSED");
        assert_eq!(wrong["misattributed"], json!(["a"]));
    });
}

#[test]
fn cargo_collector_reads_full_stdout_and_refuses_python_nodeids() {
    let _case = Case::new();
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let sink = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>, _| -> PyResult<&'static str> {
                args.get_item(1)?
                    .call1(("test receipt::tests::test_alpha ... FAILED\n",))?;
                Ok("result")
            },
        )
        .unwrap();
        let options = PyDict::new(py);
        options.set_item("argv", ("cargo", "test")).unwrap();
        options.set_item("ranked_nodeids", vec![ALPHA]).unwrap();
        options.set_item("run_command", &sink).unwrap();
        let result = value
            .getattr("collect_cargo_libtest_batch")
            .unwrap()
            .call((), Some(&options))
            .unwrap();
        assert_eq!(text(&result.get_item(0).unwrap()), "result");
        let unbound = from_python(&result.get_item(1).unwrap());
        assert_eq!(unbound["status"], "INCOMPLETE");
        assert!(unbound["error"].as_str().unwrap().contains("provenance"));
        options
            .set_item(
                "ranked_nodeids",
                vec!["conductor/test_mutation_value.py::test_alpha"],
            )
            .unwrap();
        let refused = from_python(
            &value
                .getattr("collect_cargo_libtest_batch")
                .unwrap()
                .call((), Some(&options))
                .unwrap()
                .get_item(1)
                .unwrap(),
        );
        assert_eq!(refused["status"], "INCOMPLETE");
        assert_eq!(refused["tests"], json!({}));
        assert_eq!(
            refused["missing_nodeids"],
            json!(["conductor/test_mutation_value.py::test_alpha"])
        );
        for field in ["ambiguous_nodeids", "unmapped_cases", "unranked_failures"] {
            assert_eq!(refused[field], json!([]), "{field}");
        }
        assert!(refused["error"].as_str().unwrap().contains("Rust nodeid"));
    });
}

#[test]
fn command_support_streams_verdict_past_stored_tail_and_on_timeout() {
    let case = Case::new();
    // The runner prunes the registry's parent when its last child exits.
    let registry = case.mkdir("pgids").join("live_pgids.json");
    Python::attach(|py| {
        let support = module(py, "conductor.mutation_testing_support");
        let value = module(py, "conductor.mutation_value");
        let testing = module(py, "conductor.mutation_testing");
        let tail: usize = testing
            .getattr("OUTPUT_TAIL_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        let captured = pyo3::types::PyList::empty(py);
        let command = module(py, "sys").getattr("executable").unwrap();
        let run = |script: &str, timeout: usize, sink: &Bound<'_, pyo3::types::PyList>| {
            let options = PyDict::new(py);
            options
                .set_item("cwd", path(py, &std::env::current_dir().unwrap()))
                .unwrap();
            options.set_item("timeout_seconds", timeout).unwrap();
            options.set_item("environment", PyDict::new(py)).unwrap();
            options
                .set_item("pin_argv", module(py, "builtins").getattr("list").unwrap())
                .unwrap();
            options
                .set_item(
                    "result_factory",
                    module(py, "builtins").getattr("dict").unwrap(),
                )
                .unwrap();
            options.set_item("output_tail_chars", tail).unwrap();
            options
                .set_item("pgid_registry", path(py, &registry))
                .unwrap();
            options
                .set_item("stdout_sink", sink.getattr("append").unwrap())
                .unwrap();
            support
                .getattr("run_command")
                .unwrap()
                .call(
                    (vec![text(&command), "-c".into(), script.into()],),
                    Some(&options),
                )
                .unwrap()
        };
        let chatter = format!("import sys\nsys.stdout.write('test receipt::tests::test_alpha ... FAILED\\n')\nsys.stdout.write('c' * {})\n", tail * 2);
        let result = run(&chatter, 120, &captured);
        assert!(!text(&result.get_item("stdout_tail").unwrap()).contains("test_alpha"));
        let full = captured.iter().map(|part| text(&part)).collect::<String>();
        let parsed = from_python(
            &value
                .getattr("parse_cargo_libtest")
                .unwrap()
                .call1((full.clone(), vec![ALPHA]))
                .unwrap(),
        );
        assert!(full.contains("test receipt::tests::test_alpha ... FAILED"));
        assert_eq!(parsed["status"], "INCOMPLETE");
        let timed = pyo3::types::PyList::empty(py);
        let script = "import sys, time\nsys.stdout.write('test receipt::tests::test_alpha ... FAILED\\n')\nsys.stdout.flush()\ntime.sleep(120)\n";
        let expired = run(script, 2, &timed);
        assert!(expired
            .get_item("timed_out")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let partial = from_python(
            &value
                .getattr("parse_cargo_libtest")
                .unwrap()
                .call1((
                    timed.iter().map(|part| text(&part)).collect::<String>(),
                    vec![ALPHA],
                ))
                .unwrap(),
        );
        assert_eq!(partial["status"], "INCOMPLETE");
    });
}

fn campaign<'py>(
    py: Python<'py>,
    nodeids: &[&str],
    argv: &[&str],
    adapter: Option<&str>,
) -> Bound<'py, PyAny> {
    let testing = module(py, "conductor.mutation_testing");
    let ranked_class = testing.getattr("RankedTest").unwrap();
    let ranked = nodeids
        .iter()
        .enumerate()
        .map(|(index, nodeid)| ranked_class.call1((index + 1, nodeid, "c", "r")).unwrap())
        .collect::<Vec<_>>();
    let fields = PyDict::new(py);
    fields.set_item("ranked_tests", ranked).unwrap();
    fields.set_item("test_argv", argv).unwrap();
    fields.set_item("timeout_seconds", 10).unwrap();
    fields.set_item("environment", PyDict::new(py)).unwrap();
    let value_analysis = adapter.map(|label| {
        let value = module(py, "conductor.mutation_value");
        let contract = value
            .getattr("ValueContract")
            .unwrap()
            .call1(("c", "critical", ("src/lib.rs",)))
            .unwrap();
        let test = value
            .getattr("ValueTest")
            .unwrap()
            .call1((nodeids[0], "c", false))
            .unwrap();
        value
            .getattr("ValueAnalysisSpec")
            .unwrap()
            .call1((
                label,
                2,
                PyTuple::new(py, [contract]).unwrap(),
                PyTuple::new(py, [test]).unwrap(),
                to_python(py, &json!({"m": "c"})),
            ))
            .unwrap()
    });
    fields.set_item("value_analysis", value_analysis).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&fields))
        .unwrap()
}

fn fake_runner<'py>(
    py: Python<'py>,
    calls: RunnerCalls,
    ctest: Option<std::path::PathBuf>,
) -> Bound<'py, PyAny> {
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              keywords: Option<&Bound<'_, PyDict>>|
              -> PyResult<&'static str> {
            let argv = args.get_item(0)?.extract::<Vec<String>>()?;
            let sink = keywords.and_then(|kw| kw.get_item("stdout_sink").ok().flatten());
            let has_sink = sink.as_ref().is_some_and(|value| !value.is_none());
            calls.lock().unwrap().push((argv, has_sink));
            if let Some(sink) = sink.filter(|value| !value.is_none()) {
                sink.call1(("test manifest::tests::test_alpha ... FAILED\n",))?;
            }
            if let Some(report) = &ctest {
                std::fs::create_dir_all(report.parent().unwrap()).unwrap();
                std::fs::write(report, CTEST).unwrap();
            }
            Ok("result")
        },
    )
    .unwrap()
    .into_any()
}

#[test]
fn rust_batches_route_to_libtest_and_value_analysis_requires_correct_adapter() {
    let case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = fake_runner(py, calls.clone(), None);
        let _patch = AttrPatch::replace(testing.as_any(), "_run_command", &runner);
        let rust = "tooling/native/conductor-native/src/mutation_manifest.rs::test_alpha";
        let execute = testing.getattr("_run_campaign_command").unwrap();
        let options = PyDict::new(py);
        options
            .set_item("snapshot_root", path(py, case.root()))
            .unwrap();
        options.set_item("report_name", "batch").unwrap();
        let good = campaign(py, &[rust], &["cargo", "test"], None);
        let result = execute.call((good,), Some(&options)).unwrap();
        assert_eq!(text(&result.get_item(0).unwrap()), "result");
        assert_eq!(
            text(&result.get_item(1).unwrap().get_item("status").unwrap()),
            "INCOMPLETE"
        );
        assert_eq!(
            calls.lock().unwrap().last().unwrap(),
            &(vec!["cargo".into(), "test".into()], true)
        );
        let attributed = campaign(py, &[rust], &["cargo", "test"], Some("cargo-libtest"));
        assert_eq!(
            text(
                &execute
                    .call((attributed,), Some(&options))
                    .unwrap()
                    .get_item(1)
                    .unwrap()
                    .get_item("status")
                    .unwrap()
            ),
            "INCOMPLETE"
        );
        let mislabeled = campaign(py, &[rust], &["cargo", "test"], Some("pytest-junit"));
        assert_error(
            py,
            execute.call((mislabeled,), Some(&options)).unwrap_err(),
            &testing.getattr("CampaignError").unwrap(),
            "cargo-libtest",
        );
        let opaque = campaign(
            py,
            &["tests/suite.js"],
            &["npm", "test"],
            Some("pytest-junit"),
        );
        assert_error(
            py,
            execute.call((opaque,), Some(&options)).unwrap_err(),
            &testing.getattr("CampaignError").unwrap(),
            "attributes failures",
        );
        let other = campaign(py, &["tests/suite.js"], &["npm", "test"], None);
        let result = execute.call((other,), Some(&options)).unwrap();
        assert_eq!(text(&result.get_item(0).unwrap()), "result");
        assert!(result.get_item(1).unwrap().is_none());
        assert_eq!(
            calls.lock().unwrap().last().unwrap(),
            &(vec!["npm".into(), "test".into()], false)
        );
    });
}

#[test]
fn c_batch_routes_to_ctest_even_when_shell_builds_first() {
    let case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = fake_runner(
            py,
            calls.clone(),
            Some(case.root().join(".mutation-value/ctest.xml")),
        );
        let _patch = AttrPatch::replace(testing.as_any(), "_run_command", &runner);
        let reset = "research/runtime/native/tests/test_profiler.c::test_reset_clears_all";
        let argv = ["sh", "-c", "cmake --build build && ctest --test-dir build"];
        let campaign = campaign(py, &[reset], &argv, None);
        let options = PyDict::new(py);
        options
            .set_item("snapshot_root", path(py, case.root()))
            .unwrap();
        options.set_item("report_name", "batch").unwrap();
        let result = testing
            .getattr("_run_campaign_command")
            .unwrap()
            .call((campaign,), Some(&options))
            .unwrap();
        assert_eq!(text(&result.get_item(0).unwrap()), "result");
        assert_eq!(
            text(
                &result
                    .get_item(1)
                    .unwrap()
                    .get_item("tests")
                    .unwrap()
                    .get_item(reset)
                    .unwrap()
                    .get_item("outcome")
                    .unwrap()
            ),
            "FAILED"
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[(argv.map(str::to_owned).to_vec(), false)]
        );
    });
}
