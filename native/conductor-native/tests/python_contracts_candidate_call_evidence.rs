#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts migrated from test_candidate_review_call_and_evidence.py.
//! The 39 expanded Python cases each retain a distinct Rust test.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PySet, PyString, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{attr_text, module, path, text, AttrPatch, Case};

fn namespace<'py>(py: Python<'py>, kwargs: &Bound<'py, PyDict>) -> Bound<'py, PyAny> {
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(kwargs))
        .unwrap()
}

fn mock<'py>(py: Python<'py>, key: &str, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(key, value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn parsed<'py>(py: Python<'py>, expression: &str) -> Bound<'py, PyAny> {
    module(py, "ast")
        .getattr("parse")
        .unwrap()
        .call1((expression,))
        .unwrap()
}

fn visitor<'py>(py: Python<'py>, relative: &str, source: &str, hot: bool) -> Bound<'py, PyAny> {
    let lines = PyString::new(py, source)
        .call_method0("splitlines")
        .unwrap();
    let checks = module(py, "conductor.candidate_review.checks");
    let visitor = checks
        .getattr("_PythonVisitor")
        .unwrap()
        .call1((relative, lines, hot))
        .unwrap();
    visitor
        .call_method1("visit", (parsed(py, source),))
        .unwrap();
    visitor
}

fn finding_rule_ids(py: Python<'_>, source: &str) -> Vec<String> {
    visitor(py, "m.py", source, false)
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|row| attr_text(&row.unwrap(), "rule_id"))
        .collect()
}

fn hotpath_lines(py: Python<'_>, source: &str, hot: bool) -> Vec<usize> {
    visitor(py, "lane.py", source, hot)
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .filter_map(|row| {
            let row = row.unwrap();
            (attr_text(&row, "rule_id") == "nested-loop-hotpath")
                .then(|| row.getattr("line").unwrap().extract().unwrap())
        })
        .collect()
}

macro_rules! call_name_case {
    ($name:ident, $expression:expr, $expected:expr) => {
        #[test]
        fn $name() {
            let _case = Case::new();
            Python::attach(|py| {
                let expr = parsed(py, $expression);
                let func = expr
                    .getattr("body")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .getattr("value")
                    .unwrap()
                    .getattr("func")
                    .unwrap();
                let checks = module(py, "conductor.candidate_review.checks");
                let name = checks
                    .getattr("_call_name")
                    .unwrap()
                    .call1((func,))
                    .unwrap();
                assert_eq!(text(&name), $expected);
            });
        }
    };
}

call_name_case!(call_name_eval_builtin, "eval(payload)", "eval");
call_name_case!(call_name_exec_builtin, "exec(payload)", "exec");
call_name_case!(call_name_os_system, "os.system(cmd)", "os.system");
call_name_case!(call_name_pickle_load, "pickle.load(handle)", "pickle.load");
call_name_case!(call_name_yaml_load, "yaml.load(stream)", "yaml.load");
call_name_case!(call_name_obj_eval, "obj.eval()", "obj.eval");
call_name_case!(call_name_self_eval, "self.eval()", "self.eval");
call_name_case!(
    call_name_torch_module_eval,
    "torch.nn.Module.eval(model)",
    "torch.nn.Module.eval"
);
call_name_case!(
    call_name_chained_eval_unresolved,
    "model.to(device).eval()",
    ""
);
call_name_case!(call_name_chained_exec_unresolved, "build().exec()", "");
call_name_case!(
    call_name_subscript_eval_unresolved,
    "registry['key'].eval()",
    ""
);
call_name_case!(call_name_binary_eval_unresolved, "(a + b).eval()", "");

macro_rules! dynamic_execution_case {
    ($name:ident, $source:expr, $expected:expr) => {
        #[test]
        fn $name() {
            let _case = Case::new();
            Python::attach(|py| {
                let flagged = finding_rule_ids(py, $source)
                    .iter()
                    .any(|rule| rule == "dynamic-execution");
                assert_eq!(flagged, $expected);
            });
        }
    };
}

dynamic_execution_case!(
    dynamic_execution_chained_eval_is_clean,
    "def f(model, device):\n    return model.to(device).eval()\n",
    false
);
dynamic_execution_case!(
    dynamic_execution_named_eval_is_clean,
    "def f(model):\n    return model.eval()\n",
    false
);
dynamic_execution_case!(
    dynamic_execution_builtin_eval_is_flagged,
    "def f(payload):\n    return eval(payload)\n",
    true
);
dynamic_execution_case!(
    dynamic_execution_builtin_exec_is_flagged,
    "def f(payload):\n    return exec(payload)\n",
    true
);
dynamic_execution_case!(
    dynamic_execution_os_system_is_flagged,
    "import os\n\n\ndef f(cmd):\n    return os.system(cmd)\n",
    true
);

fn evidence_context<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("snapshot", path(py, root)).unwrap();
    namespace(py, &kwargs)
}

fn registry(case: &Case) {
    case.write(
        "conductor/mutation_campaigns/registry.json",
        "{\"campaigns\": []}",
    );
}

fn evidence_payload<'py>(py: Python<'py>, covered: &str) -> Bound<'py, PyAny> {
    let row = PyDict::new(py);
    row.set_item("path", covered).unwrap();
    let payload = PyDict::new(py);
    payload.set_item("evidence", vec![row]).unwrap();
    payload.into_any()
}

fn selected(py: Python<'_>) -> Bound<'_, PySet> {
    PySet::new(py, ["test_thing.py"]).unwrap()
}

#[test]
fn property_regex_still_satisfies_the_gate() {
    let case = Case::new();
    case.write(
        "test_thing.py",
        "import pytest\n\n\n@pytest.mark.parametrize('n', [1])\ndef test_thing(n):\n    assert n\n",
    );
    Python::attach(|py| {
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("_has_property_evidence")
            .unwrap()
            .call1((evidence_context(py, case.root()), selected(py)))
            .unwrap();
        assert!(result.is_truthy().unwrap());
    });
}

#[test]
fn mutation_evidence_satisfies_gate_without_property_text() {
    let case = Case::new();
    case.write("test_thing.py", "def test_thing():\n    assert True\n");
    registry(&case);
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let context = evidence_context(py, case.root());
        let has_evidence = verification.getattr("_has_property_evidence").unwrap();
        assert!(!has_evidence
            .call1((&context, selected(py)))
            .unwrap()
            .is_truthy()
            .unwrap());
        let fake = mock(py, "return_value", &evidence_payload(py, "test_thing.py"));
        let _patch = AttrPatch::replace(
            &module(py, "conductor.mutation_testing").into_any(),
            "verify_evidence",
            &fake,
        );
        assert!(has_evidence
            .call1((context, selected(py)))
            .unwrap()
            .is_truthy()
            .unwrap());
        let args = fake.getattr("call_args").unwrap().getattr("args").unwrap();
        assert_eq!(
            args.get_item(1).unwrap().extract::<Vec<String>>().unwrap(),
            ["test_thing.py"]
        );
    });
}

macro_rules! covered_path_case {
    ($name:ident, $covered:expr, $expected:expr) => {
        #[test]
        fn $name() {
            let case = Case::new();
            registry(&case);
            Python::attach(|py| {
                let fake = mock(py, "return_value", &evidence_payload(py, $covered));
                let _patch = AttrPatch::replace(
                    &module(py, "conductor.mutation_testing").into_any(),
                    "verify_evidence",
                    &fake,
                );
                let result = module(py, "conductor.candidate_review.verification")
                    .getattr("_has_mutation_evidence")
                    .unwrap()
                    .call1((evidence_context(py, case.root()), selected(py)))
                    .unwrap();
                assert_eq!(result.is_truthy().unwrap(), $expected);
            });
        }
    };
}

covered_path_case!(
    mutation_evidence_selected_path_counts,
    "test_thing.py",
    true
);
covered_path_case!(
    mutation_evidence_unrelated_path_does_not_count,
    "test_unrelated.py",
    false
);

#[test]
fn missing_registry_is_not_mutation_evidence() {
    let case = Case::new();
    Python::attach(|py| {
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("_has_mutation_evidence")
            .unwrap()
            .call1((evidence_context(py, case.root()), selected(py)))
            .unwrap();
        assert!(!result.is_truthy().unwrap());
    });
}

#[test]
fn broken_registry_does_not_claim_evidence() {
    let case = Case::new();
    registry(&case);
    Python::attach(|py| {
        let mutation = module(py, "conductor.mutation_testing");
        let error = mutation
            .getattr("CampaignError")
            .unwrap()
            .call1(("registry is broken",))
            .unwrap();
        let fake = mock(py, "side_effect", &error);
        let _patch = AttrPatch::replace(&mutation.into_any(), "verify_evidence", &fake);
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("_has_mutation_evidence")
            .unwrap()
            .call1((evidence_context(py, case.root()), selected(py)))
            .unwrap();
        assert!(!result.is_truthy().unwrap());
    });
}

fn command_policy<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (name, value) in [("check_id", "jscpd"), ("kind", "command")] {
        kwargs.set_item(name, value).unwrap();
    }
    kwargs.set_item("profiles", ("full",)).unwrap();
    kwargs.set_item("classes", ("python",)).unwrap();
    kwargs
        .set_item("exclude_classes", PyTuple::empty(py))
        .unwrap();
    kwargs.set_item("command", ("jscpd-audit",)).unwrap();
    kwargs
        .set_item("version_command", ("jscpd", "--version"))
        .unwrap();
    kwargs
        .set_item(
            "severity",
            module(py, "conductor.candidate_review.model")
                .getattr("Severity")
                .unwrap()
                .getattr("HIGH")
                .unwrap(),
        )
        .unwrap();
    for (name, value) in [
        ("timeout_seconds", 30),
        ("memory_mb", 512),
        ("max_output_chars", 30_000),
    ] {
        kwargs.set_item(name, value).unwrap();
    }
    kwargs.set_item("always", true).unwrap();
    kwargs.set_item("cache", false).unwrap();
    kwargs.set_item("run_on_deletions", false).unwrap();
    module(py, "conductor.candidate_review.policy")
        .getattr("CheckPolicy")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn command_context<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let candidate = PyDict::new(py);
    candidate.set_item("base_commit_oid", py.None()).unwrap();
    candidate.set_item("base_tree_oid", "base-tree").unwrap();
    candidate.set_item("tree_oid", "candidate-tree").unwrap();
    let context = PyDict::new(py);
    context.set_item("repo", path(py, root)).unwrap();
    context.set_item("snapshot", path(py, root)).unwrap();
    context
        .set_item("candidate", namespace(py, &candidate))
        .unwrap();
    namespace(py, &context)
}

#[test]
fn jscpd_fingerprint_ignores_audit_roots_and_pair_order() {
    let case = Case::new();
    Python::attach(|py| {
        let runner = module(py, "conductor.candidate_review.command_runner");
        let pair_a = "  conductor/a.py  <->  conductor/b.py  (17 lines)";
        let pair_b = "  research/tools/x.py  <->  research/tools/y.py  (31 lines)";
        let completions = PyList::empty(py);
        for (root, head, pairs) in [
            ("one", "1".repeat(40), [pair_a, pair_b]),
            ("two", "2".repeat(40), [pair_b, pair_a]),
        ] {
            let output = format!("audit-root: /tmp/audit-{root} | git-head: {head} | mode: index-snapshot\ncommand: jscpd --config /tmp/{root}/jscpd.json\njscpd: 400 duplicate pair(s) found, 398 in baseline, 2 new.\nERROR: jscpd found 2 new duplicate pair(s) CAUSED by this candidate's changed files:\n{}\n{}\nRefactor to remove the duplication.", pairs[0], pairs[1]);
            let completed = module(py, "subprocess")
                .getattr("CompletedProcess")
                .unwrap()
                .call1((vec!["jscpd-audit"], 1, output, ""))
                .unwrap();
            completions.append(completed).unwrap();
        }
        let files = PyList::new(py, ["a.py"]).unwrap();
        let _files = AttrPatch::replace(
            &runner,
            "files_for_policy",
            &mock(py, "return_value", &files.into_any()),
        );
        let _run = AttrPatch::replace(
            &runner,
            "_run_process",
            &mock(py, "side_effect", &completions.into_any()),
        );
        let kwargs = PyDict::new(py);
        kwargs.set_item("version", "4.2.1").unwrap();
        let check = command_policy(py);
        let context = command_context(py, case.root());
        let call = runner.getattr("run_command_check").unwrap();
        let first = call.call((&context, &check), Some(&kwargs)).unwrap();
        let second = call.call((context, check), Some(&kwargs)).unwrap();
        let a = first.getattr("findings").unwrap().get_item(0).unwrap();
        let b = second.getattr("findings").unwrap().get_item(0).unwrap();
        assert_ne!(attr_text(&a, "message"), attr_text(&b, "message"));
        let fingerprint = attr_text(&a, "fingerprint");
        assert!(!fingerprint.is_empty());
        assert_eq!(fingerprint, attr_text(&b, "fingerprint"));
    });
}

fn row(module_name: &str, verdict: &str) -> Value {
    json!({"module": module_name, "qualname": "Lane.forward", "rule": "drop_where",
        "lineno": 12, "verdict": verdict, "description": "torch.where(...) collapsed",
        "amplifier": "params_x1e3", "max_diff_amplified": 0.97, "drivers_source": "index"})
}

fn blocking(module_name: &str) -> Value {
    row(module_name, "REACHABLE_BUT_UNTESTED")
}

/// The summary JSON `slop_core.equivalence_gate` returns, holding these rows.
fn probe_summary(blocking: Vec<Value>, advisory: Vec<Value>, modules_probed: usize) -> String {
    json!({"modules_probed": modules_probed, "measured": [], "modules_without_drivers": [],
        "modules_index_driven": [], "blocking": blocking, "advisory": advisory,
        "untested": [], "unreached": [], "incomplete": [], "unmeasured": [], "live": 0})
    .to_string()
}

fn answering<'py>(py: Python<'py>, summary: String) -> Bound<'py, PyAny> {
    mock(py, "return_value", PyString::new(py, &summary).as_any())
}

/// The check's result for a change to `names`, each changing line 12, with `fake` in
/// place of the probe, and the stand-in that answered which lines changed.
fn run_probe<'py>(
    py: Python<'py>,
    root: &Path,
    names: &[&str],
    fake: &Bound<'py, PyAny>,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let changes = PyList::empty(py);
    let lines = PyDict::new(py);
    for name in names {
        let is_test = name.rsplit('/').next().unwrap().starts_with("test_");
        let classes: &[&str] = if is_test {
            &["python", "test"]
        } else {
            &["python"]
        };
        let kwargs = PyDict::new(py);
        kwargs.set_item("path", name).unwrap();
        kwargs
            .set_item("classes", PyTuple::new(py, classes).unwrap())
            .unwrap();
        changes.append(namespace(py, &kwargs)).unwrap();
        if !is_test {
            lines.set_item(name, PySet::new(py, [12]).unwrap()).unwrap();
        }
    }
    let candidate = PyDict::new(py);
    candidate.set_item("tree_oid", "a".repeat(40)).unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, root)).unwrap();
    kwargs.set_item("snapshot", path(py, root)).unwrap();
    kwargs
        .set_item("candidate", namespace(py, &candidate))
        .unwrap();
    kwargs.set_item("live_changes", changes).unwrap();
    let tree = PyTuple::new(
        py,
        [path(py, root).into_any(), PyTuple::empty(py).into_any()],
    )
    .unwrap();
    let scoped = module(py, "contextlib")
        .getattr("nullcontext")
        .unwrap()
        .call1((tree,))
        .unwrap();
    let check = module(py, "conductor.candidate_review.equivalence_probe_check");
    let _materialize = AttrPatch::replace(
        &check,
        "materialize_tree",
        &mock(py, "return_value", &scoped),
    );
    let changed = mock(py, "return_value", lines.as_any());
    let coverage = module(py, "conductor.candidate_review.coverage_eval");
    let _lines = AttrPatch::replace(&coverage, "changed_line_numbers", &changed);
    let _probe = AttrPatch::replace(&module(py, "conductor.equivalence_gate"), "probe", fake);
    let result = check
        .getattr("check_equivalence_probe")
        .unwrap()
        .call1((namespace(py, &kwargs),))
        .unwrap();
    (result, changed)
}

fn finding_paths(result: &Bound<'_, PyAny>) -> Vec<String> {
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|row| attr_text(&row.unwrap(), "path"))
        .collect()
}

fn severity_of<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.model")
        .getattr("Severity")
        .unwrap()
        .getattr(name)
        .unwrap()
}

#[test]
fn equivalence_probe_reports_only_reachable_untested_branches() {
    let case = Case::new();
    Python::attach(|py| {
        let summary = probe_summary(
            vec![blocking("lane.py")],
            vec![row("other.py", "NO_DIFFERENCE_OBSERVED")],
            1,
        );
        let (result, _) = run_probe(py, case.root(), &["lane.py"], &answering(py, summary));
        assert_eq!(finding_paths(&result), ["lane.py"]);
        let advisory: usize = result
            .getattr("metrics")
            .unwrap()
            .get_item("advisory")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(advisory, 1);
    });
}

#[test]
fn equivalence_probe_never_probes_a_test_file() {
    let case = Case::new();
    Python::attach(|py| {
        let fake = answering(py, probe_summary(vec![], vec![], 0));
        let (_, changed) = run_probe(py, case.root(), &["lane.py", "test_lane.py"], &fake);
        let asked: Vec<String> = changed
            .getattr("call_args")
            .unwrap()
            .getattr("args")
            .unwrap()
            .get_item(2)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(asked, ["lane.py"]);
        let probed: Vec<(String, Vec<usize>)> = fake
            .getattr("call_args")
            .unwrap()
            .getattr("args")
            .unwrap()
            .get_item(2)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(probed, [("lane.py".to_owned(), vec![12])]);
        let count: usize = fake.getattr("call_count").unwrap().extract().unwrap();
        assert_eq!(count, 1);
    });
}

#[test]
fn severity_is_the_same_wherever_the_module_lives() {
    let case = Case::new();
    Python::attach(|py| {
        let modules = ["conductor/lane.py", "research/tools/one_off.py"];
        let summary = probe_summary(modules.iter().map(|m| blocking(m)).collect(), vec![], 2);
        let (result, _) = run_probe(py, case.root(), &modules, &answering(py, summary));
        let high = severity_of(py, "HIGH");
        let findings = result.getattr("findings").unwrap();
        assert_eq!(findings.len().unwrap(), 2);
        for finding in findings.try_iter().unwrap() {
            assert!(finding.unwrap().getattr("severity").unwrap().is(&high));
        }
    });
}

#[test]
fn gate_hands_findings_to_backlog_as_complete_artifact() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.forge.equivalence_probe]\nrun_artifacts = \"runs\"\n",
    );
    Python::attach(|py| {
        let rows = vec![blocking("conductor/lane.py")];
        let summary = probe_summary(rows.clone(), vec![], 1);
        run_probe(
            py,
            case.root(),
            &["conductor/lane.py"],
            &answering(py, summary),
        );
        let written: Vec<_> = fs::read_dir(case.root().join("runs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0].extension().unwrap(), "json");
        let payload: Value = serde_json::from_slice(&fs::read(&written[0]).unwrap()).unwrap();
        assert_eq!(payload["blocking"], json!(rows));
    });
}

#[test]
fn backlog_write_failure_does_not_fail_review() {
    let case = Case::new();
    case.write("unwritable", "not a directory");
    case.write(
        "pyproject.toml",
        "[tool.forge.equivalence_probe]\nrun_artifacts = \"unwritable/nested\"\n",
    );
    Python::attach(|py| {
        let summary = probe_summary(vec![blocking("conductor/lane.py")], vec![], 1);
        let (result, _) = run_probe(
            py,
            case.root(),
            &["conductor/lane.py"],
            &answering(py, summary),
        );
        let findings = result.getattr("findings").unwrap();
        assert_eq!(finding_paths(&result)[0], "conductor/lane.py");
        let unwritten = findings.get_item(1).unwrap();
        assert_eq!(attr_text(&unwritten, "rule_id"), "run-artifact-unwritten");
        assert!(unwritten
            .getattr("severity")
            .unwrap()
            .is(severity_of(py, "MEDIUM")));
    });
}

#[test]
fn missing_engine_is_named_critical_finding_without_probe_run() {
    let case = Case::new();
    Python::attach(|py| {
        let unavailable = PyString::new(py, "slop_core is not installed (test)");
        let _native = AttrPatch::replace(
            &module(py, "conductor._native"),
            "SLOP_CORE_UNAVAILABLE",
            unavailable.as_any(),
        );
        let error = module(py, "builtins")
            .getattr("AssertionError")
            .unwrap()
            .call1(("the probe ran without its engine",))
            .unwrap();
        let fake = mock(py, "side_effect", &error);
        let (result, _) = run_probe(py, case.root(), &["conductor/lane.py"], &fake);
        let findings = result.getattr("findings").unwrap();
        assert_eq!(findings.len().unwrap(), 1);
        let finding = findings.get_item(0).unwrap();
        assert_eq!(attr_text(&finding, "rule_id"), "slop-core-unavailable");
        assert!(finding
            .getattr("severity")
            .unwrap()
            .is(severity_of(py, "CRITICAL")));
        assert!(attr_text(&finding, "message").contains("slop_core is not installed (test)"));
        assert_eq!(result.getattr("files").unwrap().len().unwrap(), 0);
        let count: usize = fake.getattr("call_count").unwrap().extract().unwrap();
        assert_eq!(count, 0);
    });
}

const KERNEL: &str = "\nimport triton\nimport triton.language as tl\n\n@triton.jit\ndef kernel(out_ptr, S: tl.constexpr):\n    for scale in tl.static_range(S):\n        for offset in tl.static_range(8):\n            tl.store(out_ptr + scale * 8 + offset, 0.0)\n";

macro_rules! kernel_decorator_case {
    ($name:ident, $decorator:expr) => {
        #[test]
        fn $name() {
            let _case = Case::new();
            Python::attach(|py| {
                let source = format!("\n{}\ndef kernel(values):\n    for row in values:\n        for column in row:\n            store(row, column)\n", $decorator);
                assert!(hotpath_lines(py, &source, true).is_empty());
            });
        }
    };
}

kernel_decorator_case!(device_kernel_bare_jit_is_clean, "@jit");
kernel_decorator_case!(device_kernel_numba_cuda_jit_is_clean, "@numba.cuda.jit");
kernel_decorator_case!(device_kernel_numba_njit_is_clean, "@numba.njit");
kernel_decorator_case!(
    device_kernel_triton_autotune_is_clean,
    "@triton.autotune(configs=[], key=['S'])"
);
kernel_decorator_case!(device_kernel_triton_jit_is_clean, "@triton.jit");

#[test]
fn triton_static_range_kernel_is_clean() {
    let _case = Case::new();
    Python::attach(|py| assert!(hotpath_lines(py, KERNEL, true).is_empty()));
}

#[test]
fn real_python_nested_loop_beside_kernel_still_reports() {
    let _case = Case::new();
    let source = format!("{KERNEL}\n\ndef accumulate(rows):\n    total = 0\n    for row in rows:\n        for value in row:\n            total += value\n    return total\n");
    Python::attach(|py| {
        let lines = hotpath_lines(py, &source, true);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            source.lines().nth(lines[0] - 1).unwrap().trim(),
            "for value in row:"
        );
    });
}

#[test]
fn undecorated_helper_after_kernel_is_not_swallowed() {
    let _case = Case::new();
    let source = format!("{KERNEL}\n\ndef plain(rows):\n    for row in rows:\n        for value in row:\n            print(value)\n");
    Python::attach(|py| assert_eq!(hotpath_lines(py, &source, true).len(), 1));
}

#[test]
fn hot_path_flag_still_gates_the_rule() {
    let _case = Case::new();
    let source = "\ndef accumulate(rows):\n    total = 0\n    for row in rows:\n        for value in row:\n            total += value\n    return total\n";
    Python::attach(|py| assert!(hotpath_lines(py, source, false).is_empty()));
}
