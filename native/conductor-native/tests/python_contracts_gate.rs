#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped Python gate's bounded compatibility API.

#[path = "python_contracts/gate_support.rs"]
#[allow(dead_code)]
mod gate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use gate_support::{default_tool, fake_executable, isolated_case, json_value, py_json, repo, tool};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyFrozenSet, PyList, PyModule, PyTuple};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn gate<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.gate")
}

fn preflight<'py>(
    py: Python<'py>,
    tools: &[Bound<'py, PyAny>],
    profile: &str,
    root: &Path,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let tools = PyTuple::new(py, tools).unwrap();
    let result = gate(py)
        .getattr("preflight_tools")
        .unwrap()
        .call1((tools, profile, path(py, root)))
        .unwrap();
    (result.get_item(0).unwrap(), result.get_item(1).unwrap())
}

fn phase<'py>(py: Python<'py>, ok: bool, detail: &str) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("name", "p").unwrap();
    kwargs.set_item("ok", ok).unwrap();
    kwargs.set_item("detail", detail).unwrap();
    gate(py)
        .getattr("PhaseResult")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn namespace<'py>(py: Python<'py>, base: &str) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("integration_base", base).unwrap();
    PyModule::import(py, "types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn pytest_scope(case: &Case) -> PathBuf {
    // The production probe creates `.bytecode-isolation` beside this directory.
    case.mkdir("pytest-scope")
}

fn pytest_phase<'py>(py: Python<'py>, scope: &Path) -> Bound<'py, PyAny> {
    let executable = PyModule::import(py, "sys")
        .unwrap()
        .getattr("executable")
        .unwrap();
    gate(py)
        .getattr("preflight_pytest_config")
        .unwrap()
        .call1((path(py, scope), executable))
        .unwrap()
}

#[test]
fn search_path_prepends_node_bin_when_present() {
    let case = isolated_case();
    let bin = case.mkdir("node_modules/.bin");
    Python::attach(|py| {
        let result: String = gate(py)
            .getattr("runner_search_path")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(result.starts_with(bin.to_str().unwrap()));
    });
}

#[test]
fn search_path_is_unchanged_when_node_bin_absent() {
    let mut case = isolated_case();
    case.set_env("PATH", "/usr/bin");
    Python::attach(|py| {
        let result: String = gate(py)
            .getattr("runner_search_path")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(result, "/usr/bin");
    });
}

#[test]
fn probe_tool_reports_a_matching_version() {
    let case = isolated_case();
    fake_executable(&case.root().join("bin"), "sample", "sample 1.2.3", 0);
    Python::attach(|py| {
        let status = gate(py)
            .getattr("probe_tool")
            .unwrap()
            .call1((default_tool(py), case.root().join("bin").to_str().unwrap()))
            .unwrap();
        assert!(status.getattr("found").unwrap().extract::<bool>().unwrap());
        assert!(status
            .getattr("matches_expected")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn probe_tool_reports_a_drifted_version() {
    let case = isolated_case();
    fake_executable(&case.root().join("bin"), "sample", "sample 9.9.9", 0);
    Python::attach(|py| {
        let status = gate(py)
            .getattr("probe_tool")
            .unwrap()
            .call1((default_tool(py), case.root().join("bin").to_str().unwrap()))
            .unwrap();
        assert!(status.getattr("found").unwrap().extract::<bool>().unwrap());
        assert!(!status
            .getattr("matches_expected")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(text(&status.getattr("version").unwrap()), "sample 9.9.9");
    });
}

#[test]
fn probe_tool_reports_a_missing_tool() {
    let case = isolated_case();
    Python::attach(|py| {
        let status = gate(py)
            .getattr("probe_tool")
            .unwrap()
            .call1((
                default_tool(py),
                case.root().join("empty").to_str().unwrap(),
            ))
            .unwrap();
        assert!(!status.getattr("found").unwrap().extract::<bool>().unwrap());
        assert!(status.getattr("resolved_path").unwrap().is_none());
    });
}

#[test]
fn probe_tool_treats_a_failing_version_command_as_versionless() {
    let case = isolated_case();
    fake_executable(&case.root().join("bin"), "sample", "boom", 1);
    Python::attach(|py| {
        let status = gate(py)
            .getattr("probe_tool")
            .unwrap()
            .call1((default_tool(py), case.root().join("bin").to_str().unwrap()))
            .unwrap();
        assert!(status.getattr("found").unwrap().extract::<bool>().unwrap());
        assert!(status.getattr("version").unwrap().is_none());
        assert!(!status
            .getattr("matches_expected")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn preflight_passes_when_every_required_tool_is_present() {
    let mut case = isolated_case();
    let bin = case.root().join("bin");
    fake_executable(&bin, "sample", "sample 1.2.3", 0);
    case.set_env("PATH", bin.to_str().unwrap());
    Python::attach(|py| {
        let (phase, _) = preflight(py, &[default_tool(py)], "full", case.root());
        assert!(phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["drifted"],
            json!([])
        );
    });
}

#[test]
fn preflight_refuses_a_drifted_version() {
    let mut case = isolated_case();
    let bin = case.root().join("bin");
    fake_executable(&bin, "sample", "sample 9.9.9", 0);
    case.set_env("PATH", bin.to_str().unwrap());
    Python::attach(|py| {
        let (phase, _) = preflight(py, &[default_tool(py)], "full", case.root());
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["drifted"],
            json!(["sample"])
        );
        let detail = text(&phase.getattr("detail").unwrap());
        assert!(detail.contains("9.9.9") && detail.contains("1.2.3"));
    });
}

#[test]
fn preflight_reports_a_missing_tool_before_a_drifted_one() {
    let mut case = isolated_case();
    let bin = case.root().join("bin");
    fake_executable(&bin, "sample", "sample 9.9.9", 0);
    case.set_env("PATH", bin.to_str().unwrap());
    Python::attach(|py| {
        let tools = [
            default_tool(py),
            tool(py, "absent", "absent", "1.2.3", &["fast", "full"]),
        ];
        let (phase, _) = preflight(py, &tools, "full", case.root());
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["missing"], json!(["absent"]));
        assert!(evidence.get("drifted").is_none());
    });
}

#[test]
fn preflight_filters_tools_by_profile() {
    let mut case = isolated_case();
    case.set_env("PATH", case.root().join("empty").to_str().unwrap());
    Python::attach(|py| {
        let restricted = tool(py, "sample", "sample", "1.2.3", &["full"]);
        let (skipped, unprobed) =
            preflight(py, std::slice::from_ref(&restricted), "fast", case.root());
        assert!(skipped.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(unprobed.len().unwrap(), 0);
        let (refused, statuses) = preflight(py, &[restricted], "full", case.root());
        assert!(!refused.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&refused.getattr("evidence").unwrap())["missing"],
            json!(["sample"])
        );
        assert_eq!(statuses.len().unwrap(), 1);
    });
}

#[test]
fn preflight_names_npm_ci_when_node_modules_is_absent() {
    let mut case = isolated_case();
    case.set_env("PATH", case.root().join("empty").to_str().unwrap());
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        for (key, value) in [
            ("tool_id", "biome"),
            ("executable", "biome"),
            ("expected_version", "2.4.15"),
            ("provided_by", "npm install --global @biomejs/biome@2.4.15"),
            ("rationale", "test"),
        ] {
            kwargs.set_item(key, value).unwrap();
        }
        kwargs
            .set_item("version_command", ("biome", "--version"))
            .unwrap();
        kwargs.set_item("required_profiles", ("full",)).unwrap();
        let policy = module(py, "conductor.candidate_review.policy")
            .getattr("ToolPolicy")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let (phase, _) = preflight(py, &[policy], "full", case.root());
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert!(text(&phase.getattr("detail").unwrap()).contains("npm ci"));
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["node_modules"],
            false
        );
    });
}

#[test]
fn export_contains_tracked_files_and_no_git_directory() {
    let case = isolated_case();
    let source = repo(&case);
    let destination = case.root().join("export/tree");
    Python::attach(|py| {
        let phase = gate(py)
            .getattr("export_tree")
            .unwrap()
            .call1((path(py, &source), "HEAD", path(py, &destination)))
            .unwrap();
        assert!(phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert!(destination.join("tracked.py").is_file());
        assert!(!destination.join(".git").exists());
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["exported_files"], evidence["tracked_files"]);
    });
}

#[test]
fn export_omits_untracked_files() {
    let case = isolated_case();
    let source = repo(&case);
    fs::write(source.join("untracked.py"), "VALUE = 2\n").unwrap();
    let destination = case.root().join("export/tree");
    Python::attach(|py| {
        gate(py)
            .getattr("export_tree")
            .unwrap()
            .call1((path(py, &source), "HEAD", path(py, &destination)))
            .unwrap();
        assert!(!destination.join("untracked.py").exists());
    });
}

#[test]
fn export_refuses_an_unknown_ref() {
    let case = isolated_case();
    let source = repo(&case);
    let destination = case.root().join("export/tree");
    Python::attach(|py| {
        let gate = gate(py);
        let error = gate
            .getattr("export_tree")
            .unwrap()
            .call1((
                path(py, &source),
                "refs/heads/does-not-exist",
                path(py, &destination),
            ))
            .unwrap_err();
        assert_error(py, error, &gate.getattr("GateRefusal").unwrap(), "");
    });
}

#[test]
fn discover_skips_vendored_configs() {
    let case = isolated_case();
    case.write("pytest.ini", "[pytest]\n");
    case.write("node_modules/pkg/pytest.ini", "[pytest]\n");
    Python::attach(|py| {
        let found = gate(py)
            .getattr("discover_pytest_configs")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let found = found.cast::<PyList>().unwrap();
        assert_eq!(found.len(), 1);
        assert!(found
            .get_item(0)
            .unwrap()
            .eq(path(py, &case.root().join("pytest.ini")))
            .unwrap());
    });
}

#[test]
fn sample_test_file_picks_the_smallest() {
    let case = isolated_case();
    case.write("test_big.py", &"x = 1\n".repeat(500));
    let small = case.write("test_small.py", "x = 1\n");
    Python::attach(|py| {
        let chosen = gate(py)
            .getattr("_sample_test_file")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(chosen.eq(path(py, &small)).unwrap());
    });
}

#[test]
fn sample_test_file_is_none_without_tests() {
    let case = isolated_case();
    Python::attach(|py| {
        let chosen = gate(py)
            .getattr("_sample_test_file")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(chosen.is_none());
    });
}

#[test]
fn pytest_config_check_fails_on_an_unparseable_addopts() {
    let case = isolated_case();
    let scope = pytest_scope(&case);
    fs::write(
        scope.join("pytest.ini"),
        "[pytest]\naddopts = --this-flag-does-not-exist\n",
    )
    .unwrap();
    Python::attach(|py| {
        let phase = pytest_phase(py, &scope);
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert!(text(&phase.getattr("detail").unwrap()).contains("do not parse"));
    });
}

#[test]
fn pytest_config_check_passes_on_a_valid_addopts() {
    let case = isolated_case();
    let scope = pytest_scope(&case);
    fs::write(scope.join("pytest.ini"), "[pytest]\naddopts = --tb=short\n").unwrap();
    Python::attach(|py| {
        let phase = pytest_phase(py, &scope);
        assert!(phase.getattr("ok").unwrap().extract::<bool>().unwrap());
    });
}

#[test]
fn the_pytest_probes_run_with_isolated_bytecode_caches() {
    let case = isolated_case();
    let scope = pytest_scope(&case);
    fs::write(scope.join("pytest.ini"), "[pytest]\naddopts = --tb=short\n").unwrap();
    Python::attach(|py| {
        let phase = pytest_phase(py, &scope);
        assert!(phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert!(case.root().join(".bytecode-isolation/pycache").is_dir());
    });
}

#[test]
fn waivers_are_active_on_their_pinned_base() {
    let _case = isolated_case();
    Python::attach(|py| {
        let waivers = PyTuple::new(py, [namespace(py, "abc123"), namespace(py, "abc123")]).unwrap();
        let phase = gate(py)
            .getattr("waiver_activation")
            .unwrap()
            .call1((waivers, "abc123"))
            .unwrap();
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["active"], 2);
        assert_eq!(evidence["inert"], 0);
        assert!(!text(&phase.getattr("detail").unwrap()).contains("inert"));
    });
}

#[test]
fn waivers_are_inert_on_any_other_base() {
    let _case = isolated_case();
    Python::attach(|py| {
        let waivers = PyTuple::new(py, [namespace(py, "abc123"), namespace(py, "abc123")]).unwrap();
        let phase = gate(py)
            .getattr("waiver_activation")
            .unwrap()
            .call1((waivers, "def456"))
            .unwrap();
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["active"], 0);
        assert_eq!(evidence["inert"], 2);
        assert!(text(&phase.getattr("detail").unwrap()).contains("inert"));
    });
}

#[test]
fn waiver_activation_is_reported_per_waiver_not_all_or_nothing() {
    let _case = isolated_case();
    Python::attach(|py| {
        let waivers = PyTuple::new(py, [namespace(py, "abc123"), namespace(py, "def456")]).unwrap();
        let phase = gate(py)
            .getattr("waiver_activation")
            .unwrap()
            .call1((waivers, "abc123"))
            .unwrap();
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["active"], 1);
        assert_eq!(evidence["inert"], 1);
    });
}

#[test]
fn render_distinguishes_pass_fail_and_refused() {
    let _case = isolated_case();
    Python::attach(|py| {
        let gate = gate(py);
        let ok = PyList::new(py, [phase(py, true, "fine")]).unwrap();
        let bad = PyList::new(py, [phase(py, false, "broken")]).unwrap();
        let statuses = PyList::empty(py);
        for (phases, exit_name, prefix) in [
            (&ok, "EXIT_PASS", "gate | PASS"),
            (&bad, "EXIT_FAIL", "gate | FAIL"),
            (&bad, "EXIT_REFUSED", "gate | REFUSED"),
        ] {
            let rendered: String = gate
                .getattr("render")
                .unwrap()
                .call1((phases, &statuses, gate.getattr(exit_name).unwrap()))
                .unwrap()
                .extract()
                .unwrap();
            assert!(rendered.starts_with(prefix));
        }
    });
}

#[test]
fn exit_codes_are_distinct() {
    let _case = isolated_case();
    Python::attach(|py| {
        let gate = gate(py);
        let codes = ["EXIT_PASS", "EXIT_FAIL", "EXIT_REFUSED"]
            .map(|name| gate.getattr(name).unwrap().extract::<i32>().unwrap());
        assert_eq!(codes.into_iter().collect::<BTreeSet<_>>().len(), 3);
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuditCall {
    registry: String,
    repo_root: String,
    summary: bool,
    changed_files: Option<BTreeSet<String>>,
}

fn audit_result(py: Python<'_>, moved: Option<(&str, &[&str])>) -> Value {
    let keys: Vec<String> = module(py, "conductor.mutation_patch_audit")
        .getattr("BASELINE_KEYS")
        .unwrap()
        .extract()
        .unwrap();
    let mut delta = Map::new();
    for key in keys {
        for side in ["new", "resolved"] {
            let name = format!("{side}_{key}");
            let ids = moved
                .filter(|(wanted, _)| *wanted == name)
                .map_or_else(Vec::new, |(_, ids)| ids.to_vec());
            delta.insert(name, json!(ids));
        }
    }
    let status = match moved {
        Some((key, ids)) if !ids.is_empty() && key.starts_with("new_") => "REGRESSED",
        Some((_, ids)) if !ids.is_empty() => "BASELINE_STALE",
        _ => "CLEAN",
    };
    delta.insert("status".into(), json!(status));
    json!({"status":status,"campaigns":492,"reproducibility":{"baseline":delta}})
}

fn export_with_registry(case: &Case, py: Python<'_>) -> PathBuf {
    let export = case.mkdir("tree");
    let relative = PathBuf::from(text(&gate(py).getattr("MUTATION_REGISTRY").unwrap()));
    assert!(!relative.is_absolute());
    assert!(relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_))));
    let registry = export.join(relative);
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(registry, "{\"campaigns\": []}").unwrap();
    export
}

fn stub_audit(py: Python<'_>, result: Value, calls: Arc<Mutex<Vec<AuditCall>>>) -> AttrPatch {
    let audit = module(py, "conductor.mutation_patch_audit");
    let fake =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            assert_eq!(args.len(), 1);
            let kwargs = kwargs.expect("audit_corpus keyword arguments");
            assert_eq!(kwargs.len(), 3);
            let root = kwargs.get_item("repo_root")?.expect("repo_root");
            let summary = kwargs.get_item("summary")?.expect("summary");
            let changed = kwargs.get_item("changed_files")?.expect("changed_files");
            let changed_files = if changed.is_none() {
                None
            } else {
                let entries = changed
                    .cast::<PyFrozenSet>()?
                    .iter()
                    .map(|entry| entry.extract::<String>())
                    .collect::<PyResult<BTreeSet<_>>>()?;
                Some(entries)
            };
            calls.lock().unwrap().push(AuditCall {
                registry: text(&args.get_item(0)?),
                repo_root: text(&root),
                summary: summary.extract()?,
                changed_files,
            });
            Ok(py_json(args.py(), result.clone()).unbind())
        })
        .unwrap();
    AttrPatch::replace(audit.as_any(), "audit_corpus", fake.as_any())
}

fn corpus_phase<'py>(
    py: Python<'py>,
    export: &Path,
    changed: Option<&Bound<'py, PyFrozenSet>>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    if let Some(changed) = changed {
        kwargs.set_item("changed_files", changed).unwrap();
    }
    gate(py)
        .getattr("mutation_corpus_audit")
        .unwrap()
        .call((path(py, export),), Some(&kwargs))
        .unwrap()
}

#[test]
fn corpus_audit_passes_at_the_recorded_baseline() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let _stub = stub_audit(py, audit_result(py, None), Arc::clone(&calls));
        let phase = corpus_phase(py, &export, None);
        assert!(phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(text(&phase.getattr("name").unwrap()), "mutation-corpus");
        let evidence = json_value(&phase.getattr("evidence").unwrap());
        assert_eq!(evidence["status"], "CLEAN");
        assert_eq!(evidence["exit_code"], 0);
        assert_eq!(calls.lock().unwrap().len(), 1);
    });
}

#[test]
fn corpus_audit_reads_the_export_not_the_working_tree() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let registry = export.join(text(&gate(py).getattr("MUTATION_REGISTRY").unwrap()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let _stub = stub_audit(py, audit_result(py, None), Arc::clone(&calls));
        corpus_phase(py, &export, None);
        let seen = calls.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].repo_root, export.display().to_string());
        assert_eq!(seen[0].registry, registry.display().to_string());
        assert!(seen[0].summary);
    });
}

#[test]
fn corpus_audit_forwards_the_candidate_diff() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let _stub = stub_audit(py, audit_result(py, None), Arc::clone(&calls));
        let changed = PyFrozenSet::new(py, ["src/one.py", "src/two.py"]).unwrap();
        corpus_phase(py, &export, Some(&changed));
        corpus_phase(py, &export, None);
        let seen = calls.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].changed_files,
            Some(["src/one.py".into(), "src/two.py".into()].into())
        );
        assert_eq!(seen[1].changed_files, None);
    });
}

#[test]
fn corpus_audit_fails_on_a_newly_rotted_mutant() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let _stub = stub_audit(
            py,
            audit_result(
                py,
                Some(("new_stale_mutations", &["some_campaign::some_mutant"])),
            ),
            Arc::new(Mutex::new(Vec::new())),
        );
        let phase = corpus_phase(py, &export, None);
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["exit_code"],
            6
        );
        assert!(text(&phase.getattr("detail").unwrap()).contains("some_campaign::some_mutant"));
    });
}

#[test]
fn corpus_audit_fails_when_a_baseline_entry_stops_failing() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let _stub = stub_audit(
            py,
            audit_result(
                py,
                Some(("resolved_stale_mutations", &["some_campaign::repaired"])),
            ),
            Arc::new(Mutex::new(Vec::new())),
        );
        let phase = corpus_phase(py, &export, None);
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["exit_code"],
            6
        );
        assert!(text(&phase.getattr("detail").unwrap()).contains("some_campaign::repaired"));
    });
}

#[test]
fn corpus_audit_fails_on_a_non_patch_regression() {
    let case = isolated_case();
    Python::attach(|py| {
        let export = export_with_registry(&case, py);
        let _stub = stub_audit(
            py,
            audit_result(py, Some(("new_tests_that_kill_nothing", &["c::test_x"]))),
            Arc::new(Mutex::new(Vec::new())),
        );
        let phase = corpus_phase(py, &export, None);
        assert!(!phase.getattr("ok").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            json_value(&phase.getattr("evidence").unwrap())["exit_code"],
            7
        );
        assert!(text(&phase.getattr("detail").unwrap()).contains("c::test_x"));
    });
}

#[test]
fn corpus_audit_refuses_when_the_candidate_has_no_registry() {
    let case = isolated_case();
    let export = case.mkdir("tree");
    Python::attach(|py| {
        let gate = gate(py);
        let error = gate
            .getattr("mutation_corpus_audit")
            .unwrap()
            .call1((path(py, &export),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &gate.getattr("GateRefusal").unwrap(),
            "no mutation registry",
        );
    });
}

#[test]
fn corpus_detail_truncates_long_id_lists_but_keeps_the_count() {
    let _case = isolated_case();
    Python::attach(|py| {
        let ids = (0..5).map(|i| format!("c::m{i}")).collect::<Vec<_>>();
        let detail: String = gate(py)
            .getattr("_corpus_detail")
            .unwrap()
            .call1((
                "REGRESSED",
                py_json(py, json!({"stale_mutations":ids})),
                PyDict::new(py),
            ))
            .unwrap()
            .extract()
            .unwrap();
        assert!(detail.contains("stale_mutations 5"));
        assert!(detail.contains("c::m0"));
        assert!(detail.contains("..."));
        assert!(!detail.contains("c::m4"));
    });
}
