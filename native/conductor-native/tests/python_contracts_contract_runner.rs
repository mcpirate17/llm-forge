#![cfg(feature = "python-compat-tests")]
//! End-to-end adapter contracts for targeted Python and Rust test execution.

#[path = "python_contracts/graph_test_select_support.rs"]
#[allow(dead_code)]
mod graph_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use graph_support::{capture, git, GraphRepo};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PySet, PyTuple};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, path, text, AttrPatch, Case};

fn forge_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn contract_fixture(repo: &GraphRepo<'_>) {
    repo.write("src/conductor/memory_vectors.py", "def probe(): return 1\n");
    repo.write(
        "native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/memory_vectors.py\tpython_contracts_memory_vectors\n",
    );
    repo.write(
        "native/conductor-native/Cargo.toml",
        "[package]\nname='probe'\n",
    );
    repo.write(
        "native/conductor-native/tests/python_contracts_memory_vectors.rs",
        "#[test] fn probe() {}\n",
    );
}

const PROVENANCE_TEST: &str = r#"
#[test]
fn candidate_imports_and_binary_are_used() {
    use std::process::Command;
    let python = std::env::var("PYO3_PYTHON").unwrap();
    let script = "import conductor, conductor._native, conductor.candidate_probe, conductor_native; print(conductor.__file__); print(conductor.candidate_probe.MARKER); print(conductor._native.marker()); print(conductor_native.__file__)";
    let imported = Command::new(python).args(["-c", script]).output().unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    let lines = String::from_utf8(imported.stdout).unwrap();
    let lines: Vec<_> = lines.lines().collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    assert_eq!(lines[0], root.join("src/conductor/__init__.py").to_str().unwrap());
    assert_eq!(lines[1], "CANDIDATE_PYTHON");
    assert_eq!(lines[2], "CANDIDATE_EXTENSION");
    assert!(lines[3].ends_with("/contract-extension/conductor_native.so"));
    let forge = Command::new(std::env::var("FORGE_BIN").unwrap()).output().unwrap();
    assert!(forge.status.success());
    assert_eq!(String::from_utf8(forge.stdout).unwrap().trim(), "CANDIDATE_FORGE");
}
"#;

const SLOP_PROVENANCE_TEST: &str = r#"
#[test]
fn candidate_slop_extension_is_used() {
    use std::process::Command;
    let python = std::env::var("PYO3_PYTHON").unwrap();
    let script = "import slop_core; print(slop_core.marker()); print(slop_core.__file__)";
    let imported = Command::new(python).args(["-c", script]).output().unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    let lines = String::from_utf8(imported.stdout).unwrap();
    let lines: Vec<_> = lines.lines().collect();
    assert_eq!(lines[0], "CANDIDATE_SLOP");
    assert!(lines[1].ends_with("/contract-extension/slop_core.so"));
}
"#;

fn generate_lock(root: &Path, manifest: &str) {
    let output = Command::new("cargo")
        .args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            manifest,
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture lockfile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn runtime_fixture(case: &Case) -> PathBuf {
    let root = case.mkdir("candidate_ws");
    case.write(
        "candidate_ws/native/conductor-native/Cargo.toml",
        "[package]\nname='conductor-native'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='conductor_native'\ncrate-type=['cdylib']\n[features]\npython=[]\npython-compat-tests=[]\n[dependencies]\npyo3={version='0.29',features=['extension-module']}\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/src/lib.rs",
        "use pyo3::prelude::*;\nuse pyo3::types::PyModule;\n#[pyfunction] fn marker() -> &'static str { \"CANDIDATE_EXTENSION\" }\n#[pymodule] fn conductor_native(module: &Bound<'_, PyModule>) -> PyResult<()> { module.add_function(wrap_pyfunction!(marker, module)?)?; Ok(()) }\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/tests/python_contracts_probe.rs",
        PROVENANCE_TEST,
    );
    case.write(
        "candidate_ws/native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/candidate_probe.py\tpython_contracts_probe\n",
    );
    case.write(
        "candidate_ws/native/forge/Cargo.toml",
        "[package]\nname='forge'\nversion='0.1.0'\nedition='2021'\n[[bin]]\nname='forge'\npath='src/main.rs'\n",
    );
    case.write(
        "candidate_ws/native/forge/src/main.rs",
        "fn main() { println!(\"CANDIDATE_FORGE\"); }\n",
    );
    case.write(
        "candidate_ws/src/conductor/__init__.py",
        "MARKER='CANDIDATE_PACKAGE'\n",
    );
    case.write(
        "candidate_ws/src/conductor/candidate_probe.py",
        "MARKER='CANDIDATE_PYTHON'\n",
    );
    case.write(
        "candidate_ws/src/conductor/_native.py",
        "from conductor_native import marker\n",
    );
    generate_lock(&root, "native/conductor-native/Cargo.toml");
    generate_lock(&root, "native/forge/Cargo.toml");
    root
}

fn slop_runtime_fixture(case: &Case) -> PathBuf {
    let root = runtime_fixture(case);
    case.write(
        "candidate_ws/native/slop-core/Cargo.toml",
        "[package]\nname='slop-core'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='slop_core'\ncrate-type=['cdylib']\n[features]\nextension-module=['pyo3/extension-module']\n[dependencies]\npyo3={version='0.29'}\n",
    );
    case.write(
        "candidate_ws/native/slop-core/src/lib.rs",
        "use pyo3::prelude::*;\nuse pyo3::types::PyModule;\n#[pyfunction] fn marker() -> &'static str { \"CANDIDATE_SLOP\" }\n#[pymodule] fn slop_core(module: &Bound<'_, PyModule>) -> PyResult<()> { module.add_function(wrap_pyfunction!(marker, module)?)?; Ok(()) }\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/tests/python_contracts_native_ablations.rs",
        SLOP_PROVENANCE_TEST,
    );
    case.write(
        "candidate_ws/native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/candidate_probe.py\tpython_contracts_native_ablations\n",
    );
    generate_lock(&root, "native/slop-core/Cargo.toml");
    root
}

fn conflicting_ambient(case: &mut Case) {
    case.write(
        "ambient/conductor/__init__.py",
        "MARKER='AMBIENT_PACKAGE'\n",
    );
    case.write(
        "ambient/conductor/candidate_probe.py",
        "MARKER='AMBIENT_PYTHON'\n",
    );
    case.write(
        "ambient/conductor/_native.py",
        "def marker(): return 'AMBIENT_EXTENSION'\n",
    );
    case.write(
        "ambient/conductor_native.py",
        "def marker(): return 'AMBIENT_EXTENSION'\n",
    );
    case.write(
        "ambient/slop_core.py",
        "def marker(): return 'AMBIENT_SLOP'\n",
    );
    case.write("ambient/forge", "#!/bin/sh\necho AMBIENT_FORGE\n");
    let ambient_path = case.root().join("ambient").display().to_string();
    let ambient_forge = case.root().join("ambient/forge").display().to_string();
    let wrong_abi = case.root().join("wrong-abi.txt").display().to_string();
    case.set_env("PYTHONPATH", &ambient_path);
    case.set_env("FORGE_BIN", &ambient_forge);
    case.set_env("PYO3_CONFIG_FILE", &wrong_abi);
}

fn runtime_contract_plan<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let native = module(py, "conductor._native");
    let raw = native
        .getattr("contract_test_plan_native")
        .unwrap()
        .call1((
            root.display().to_string(),
            vec!["src/conductor/candidate_probe.py"],
        ))
        .unwrap();
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((raw,))
        .unwrap()
}

fn native_runtime_plan(py: Python<'_>, root: &Path, targets: &[&str]) -> PyResult<Value> {
    let request = json!({
        "snapshot": root,
        "runtime_dir": root.join("runtime"),
        "python_executable": text(&module(py, "sys").getattr("executable").unwrap()),
        "targets": targets,
    });
    let response: String = module(py, "conductor._native")
        .getattr("candidate_verification_native")?
        .call1(("contract_runtime_plan", request.to_string()))?
        .extract()?;
    Ok(serde_json::from_str(&response).unwrap())
}

#[test]
fn slop_build_is_selected_only_for_observed_consumers() {
    let case = Case::new();
    let root = slop_runtime_fixture(&case);
    Python::attach(|py| {
        for (target, expected) in [
            ("python_contracts_probe", false),
            ("python_contracts_native_ablations", true),
            ("python_contracts_candidate_style", true),
            ("python_contracts_policy_engine_crash", true),
            ("python_contracts_probe_budget", true),
            ("python_contracts_slop_gate", true),
            ("python_contracts_slop_ledger", true),
            ("python_contracts_reuse_inventory", true),
            ("python_contracts_reuse_roi", true),
            ("python_contracts_reuse_consolidation", true),
            ("python_contracts_reuse_file_families", true),
            ("python_contracts_reuse_audit_inventory", true),
            ("python_contracts_reuse_file_family_lsh", true),
            ("python_contracts_reuse_consolidation_collect", true),
            ("python_contracts_reuse_detector_scan", true),
            ("python_contracts_reuse_file_family_profiles", true),
        ] {
            let plan = native_runtime_plan(py, &root, &[target]).unwrap();
            let builds = plan["build_commands"].as_array().unwrap();
            assert_eq!(builds.len(), if expected { 3 } else { 2 });
            let slop = builds.iter().find(|step| {
                step["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "native/slop-core/Cargo.toml")
            });
            assert_eq!(slop.is_some(), expected);
            if let Some(step) = slop {
                assert_eq!(
                    step["artifact"],
                    json!(root.join("runtime/contract-cargo-target/debug/libslop_core.so"))
                );
                assert_eq!(
                    step["destination"],
                    json!(root.join("runtime/contract-extension/slop_core.so"))
                );
                let argv = step["argv"].as_array().unwrap();
                for arg in [
                    "--offline",
                    "--locked",
                    "--jobs",
                    "2",
                    "--features",
                    "extension-module",
                    "--lib",
                ] {
                    assert!(argv.iter().any(|value| value == arg));
                }
            }
        }
    });
}

#[test]
fn selected_slop_requires_a_regular_candidate_manifest() {
    let case = Case::new();
    let root = slop_runtime_fixture(&case);
    let manifest = root.join("native/slop-core/Cargo.toml");
    std::fs::remove_file(&manifest).unwrap();
    Python::attach(|py| {
        let error =
            native_runtime_plan(py, &root, &["python_contracts_native_ablations"]).unwrap_err();
        assert!(error.to_string().contains("candidate slop_core"));
        assert!(native_runtime_plan(py, &root, &["python_contracts_probe"]).is_ok());
    });
    let outside = case.write("outside-slop.toml", "[package]\nname='outside'\n");
    std::os::unix::fs::symlink(outside, &manifest).unwrap();
    Python::attach(|py| {
        let error =
            native_runtime_plan(py, &root, &["python_contracts_candidate_style"]).unwrap_err();
        assert!(error.to_string().contains("candidate slop_core"));
    });
}

fn runtime_check<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let args = PyDict::new(py);
    args.set_item("check_id", "targeted-tests").unwrap();
    args.set_item("shard_max_files", 8).unwrap();
    args.set_item("shard_workers", 1).unwrap();
    args.set_item("timeout_seconds", 900).unwrap();
    args.set_item("wall_timeout_seconds", 900).unwrap();
    args.set_item("memory_mb", 16384).unwrap();
    args.set_item("max_output_chars", 4000).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&args))
        .unwrap()
}

fn mock_graph_cargo<'py>(
    py: Python<'py>,
    graph: &Bound<'py, pyo3::types::PyModule>,
    repo: &GraphRepo<'_>,
) -> (Bound<'py, PyAny>, Vec<AttrPatch>) {
    let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
    let process_args = PyDict::new(py);
    process_args.set_item("returncode", 0).unwrap();
    let process = module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&process_args))
        .unwrap();
    let result_args = PyDict::new(py);
    result_args.set_item("return_value", process).unwrap();
    let subprocess_run = mock.call((), Some(&result_args)).unwrap();
    let subprocess = graph.getattr("subprocess").unwrap();
    let run_guard = AttrPatch::replace(&subprocess, "run", &subprocess_run);
    let no_build = PyDict::new(py);
    no_build
        .set_item(
            "return_value",
            module(py, "contextlib")
                .getattr("nullcontext")
                .unwrap()
                .call1((PyDict::new(py),))
                .unwrap(),
        )
        .unwrap();
    let runtime = mock.call((), Some(&no_build)).unwrap();
    let runtime_guard = AttrPatch::replace(graph, "standalone_contract_runtime", &runtime);
    let root_kwargs = PyDict::new(py);
    root_kwargs
        .set_item("return_value", path(py, repo.root()))
        .unwrap();
    let root_lookup = mock.call((), Some(&root_kwargs)).unwrap();
    let root_guard = AttrPatch::replace(graph, "repository_root", &root_lookup);
    (subprocess_run, vec![run_guard, runtime_guard, root_guard])
}

#[test]
fn candidate_selection_includes_renamed_source_and_keeps_pytest_separate() {
    let _case = Case::new();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let types = module(py, "types");
        let namespace = types.getattr("SimpleNamespace").unwrap();
        let change_args = PyDict::new(py);
        change_args
            .set_item("path", "src/conductor/active_state.py")
            .unwrap();
        change_args
            .set_item("old_path", "src/conductor/memory_vectors.py")
            .unwrap();
        change_args.set_item("classes", vec!["python"]).unwrap();
        change_args.set_item("deleted", false).unwrap();
        change_args.set_item("risk", "normal").unwrap();
        let change = namespace.call((), Some(&change_args)).unwrap();
        let context_args = PyDict::new(py);
        context_args
            .set_item("snapshot", path(py, &forge_root()))
            .unwrap();
        let changes = PyTuple::new(py, [change]).unwrap();
        context_args.set_item("live_changes", &changes).unwrap();
        let candidate_args = PyDict::new(py);
        candidate_args.set_item("changes", &changes).unwrap();
        context_args
            .set_item(
                "candidate",
                namespace.call((), Some(&candidate_args)).unwrap(),
            )
            .unwrap();
        let context = namespace.call((), Some(&context_args)).unwrap();
        let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
        let graph_kwargs = PyDict::new(py);
        graph_kwargs
            .set_item("return_value", (PySet::empty(py).unwrap(), PyDict::new(py)))
            .unwrap();
        let graph = mock.call((), Some(&graph_kwargs)).unwrap();
        let _graph = AttrPatch::replace(&verification, "_graph_test_paths", &graph);
        let empty_kwargs = PyDict::new(py);
        empty_kwargs
            .set_item("return_value", PySet::empty(py).unwrap())
            .unwrap();
        let empty = mock.call((), Some(&empty_kwargs)).unwrap();
        let _convention = AttrPatch::replace(&verification, "_convention_tests", &empty);
        let native_kwargs = PyDict::new(py);
        native_kwargs
            .set_item("return_value", PyDict::new(py))
            .unwrap();
        let no_native = mock.call((), Some(&native_kwargs)).unwrap();
        let _native = AttrPatch::replace(&verification, "_rust_crate_tests", &no_native);
        let selection = verification
            .getattr("select_tests")
            .unwrap()
            .call1((context,))
            .unwrap();
        assert_eq!(selection.getattr("tests").unwrap().len().unwrap(), 0);
        assert_eq!(selection.getattr("findings").unwrap().len().unwrap(), 0);
        let plan = selection.getattr("contract_plan").unwrap();
        assert_eq!(
            plan.get_item("targets")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec![
                "python_contracts_active_state",
                "python_contracts_crg_workspace_tools",
                "python_contracts_inplace_handoff",
                "python_contracts_installed_layout",
                "python_contracts_local_ai_policy",
                "python_contracts_memory_vectors",
                "python_contracts_session_preamble",
                "python_contracts_workspace_eval",
                "python_contracts_workspace_runtime_reconcile"
            ]
        );
        let graph = selection.getattr("graph").unwrap();
        assert_eq!(
            graph
                .get_item("contract_test_files")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            9
        );
    });
}

#[test]
fn native_policy_reports_unmapped_source_in_mixed_contract_change() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        let request = json!({
            "sources": ["src/conductor/memory_vectors.py", "src/conductor/unmapped.py"],
            "changed_tests": [],
            "graph_tests": [],
            "convention_tests": [],
            "native_tests": {},
            "contract_sources": ["src/conductor/memory_vectors.py"],
            "contract_targets": ["python_contracts_memory_vectors"],
            "contract_test_paths": ["native/conductor-native/tests/python_contracts_memory_vectors.rs"],
            "graph": {},
            "graph_error": null,
            "high_risk": false,
            "property_evidence": false
        });
        let raw = native
            .getattr("candidate_verification_native")
            .unwrap()
            .call1(("selection_decide", request.to_string()))
            .unwrap();
        let response: Value = serde_json::from_str(&text(&raw)).unwrap();
        assert_eq!(response["findings"][0]["rule_id"], "no-targeted-tests");
        assert_eq!(
            response["findings"][0]["evidence"]["source_paths"],
            json!(["src/conductor/unmapped.py"])
        );
        assert_eq!(response["graph"]["contract_test_files"], 1);
    });
}

#[test]
fn graph_cli_exposes_and_launches_cargo_without_passing_rust_to_pytest() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    contract_fixture(&repo);
    Python::attach(|py| {
        let graph = module(py, "conductor.graph_test_select");
        let root = repo.root().to_string_lossy().into_owned();
        let args = vec!["--repo", &root, "--json", "src/conductor/memory_vectors.py"];
        let (code, stdout, _) = capture(py, || {
            graph
                .getattr("main")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(code, 0);
        let result: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["selected_tests"], json!([]));
        assert_eq!(
            result["contract_targets"],
            json!(["python_contracts_memory_vectors"])
        );
        assert_eq!(result["count"], 1);
        let (subprocess_run, _guards) = mock_graph_cargo(py, &graph, &repo);
        let (code, _, _) = capture(py, || {
            graph
                .getattr("main")
                .unwrap()
                .call1((vec![
                    "--repo",
                    &root,
                    "--run",
                    "src/conductor/memory_vectors.py",
                ],))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(code, 0);
        let calls = subprocess_run.getattr("call_args_list").unwrap();
        assert_eq!(calls.len().unwrap(), 1);
        assert_eq!(
            calls
                .get_item(0)
                .unwrap()
                .getattr("kwargs")
                .unwrap()
                .get_item("timeout")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            900
        );
        let argv: Vec<String> = calls
            .get_item(0)
            .unwrap()
            .getattr("args")
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(argv[0], "cargo");
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--test", "python_contracts_memory_vectors"]));
        assert!(!argv.iter().any(|arg| arg.ends_with(".rs")));
        subprocess_run
            .getattr("return_value")
            .unwrap()
            .setattr("returncode", 3)
            .unwrap();
        let (failed, _, _) = capture(py, || {
            graph
                .getattr("main")
                .unwrap()
                .call1((vec![
                    "--repo",
                    &root,
                    "--run",
                    "src/conductor/memory_vectors.py",
                ],))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(failed, 3);
    });
}

#[test]
fn default_graph_cli_diff_keeps_both_sides_of_a_rename() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("src/conductor/memory_vectors.py", "MARKER = 'old'\n");
    git(repo.root(), &["add", "src/conductor/memory_vectors.py"]);
    git(repo.root(), &["commit", "-m", "add probe", "--quiet"]);
    git(
        repo.root(),
        &[
            "mv",
            "src/conductor/memory_vectors.py",
            "src/conductor/renamed_vectors.py",
        ],
    );
    Python::attach(|py| {
        let changed: Vec<String> = module(py, "conductor.graph_test_select")
            .getattr("git_changed_and_untracked_files")
            .unwrap()
            .call1((path(py, repo.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(changed.contains(&"src/conductor/memory_vectors.py".to_owned()));
        assert!(changed.contains(&"src/conductor/renamed_vectors.py".to_owned()));
    });
}

fn mixed_selection<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let raw = module(py, "conductor._native")
        .getattr("contract_test_plan_native")
        .unwrap()
        .call1((
            forge_root().display().to_string(),
            vec!["src/conductor/memory_vectors.py"],
        ))
        .unwrap();
    let plan = module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((raw,))
        .unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("TestSelection")
        .unwrap()
        .call1((
            vec!["test_probe.py"],
            PyDict::new(py),
            PyTuple::empty(py),
            plan,
        ))
        .unwrap()
}

fn mixed_context_check<'py>(
    py: Python<'py>,
    case: &Case,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let namespace = module(py, "types").getattr("SimpleNamespace").unwrap();
    let context_args = PyDict::new(py);
    context_args
        .set_item("snapshot", path(py, case.root()))
        .unwrap();
    context_args
        .set_item("runtime_dir", path(py, case.root()))
        .unwrap();
    let context = namespace.call((), Some(&context_args)).unwrap();
    let check_args = PyDict::new(py);
    check_args
        .set_item("check_id", "targeted-tests-full")
        .unwrap();
    check_args.set_item("shard_max_files", 8).unwrap();
    check_args.set_item("shard_workers", 2).unwrap();
    check_args.set_item("max_output_chars", 1000).unwrap();
    let check = namespace.call((), Some(&check_args)).unwrap();
    (context, check)
}

fn mixed_execution_mocks<'py>(
    py: Python<'py>,
    verification: &Bound<'py, pyo3::types::PyModule>,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>, Vec<AttrPatch>) {
    let subprocess = module(py, "subprocess");
    let complete = subprocess.getattr("CompletedProcess").unwrap();
    let ok = complete.call1((PyList::empty(py), 0, "ok", "")).unwrap();
    let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
    let mock_args = PyDict::new(py);
    mock_args
        .set_item("return_value", (vec![ok.clone(), ok], Vec::<usize>::new()))
        .unwrap();
    let execute = mock.call((), Some(&mock_args)).unwrap();
    let execute_guard = AttrPatch::replace(verification, "execute_shards", &execute);
    let no_build = PyDict::new(py);
    no_build.set_item("return_value", PyDict::new(py)).unwrap();
    let runtime = mock.call((), Some(&no_build)).unwrap();
    let runtime_guard = AttrPatch::replace(verification, "prepare_contract_runtime", &runtime);
    let coverage_args = PyDict::new(py);
    let coverage_metrics = PyDict::new(py);
    coverage_metrics
        .set_item("measured_scope", "pytest-only")
        .unwrap();
    coverage_args
        .set_item("return_value", (PyList::empty(py), coverage_metrics))
        .unwrap();
    let coverage_check = mock.call((), Some(&coverage_args)).unwrap();
    let coverage_guard =
        AttrPatch::replace(verification, "_evaluate_changed_coverage", &coverage_check);
    (
        execute,
        coverage_check,
        vec![execute_guard, runtime_guard, coverage_guard],
    )
}

#[test]
fn candidate_runner_executes_mixed_commands_and_reports_coverage_limit() {
    let case = Case::new();
    Python::attach(|py| {
        let selection = mixed_selection(py);
        let (context, check) = mixed_context_check(py, &case);
        let verification = module(py, "conductor.candidate_review.verification");
        let (execute, coverage_check, _guards) = mixed_execution_mocks(py, &verification);
        let kwargs = PyDict::new(py);
        kwargs.set_item("coverage", true).unwrap();
        let result = verification
            .getattr("run_targeted_tests")
            .unwrap()
            .call((&context, &selection, &check), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&result.getattr("status").unwrap()), "failed");
        assert_eq!(
            coverage_check
                .getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert_eq!(
            result
                .getattr("metrics")
                .unwrap()
                .get_item("measured_scope")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "pytest-only"
        );
        let finding = result.getattr("findings").unwrap().get_item(0).unwrap();
        assert_eq!(
            text(&finding.getattr("rule_id").unwrap()),
            "rust-contract-coverage-unsupported"
        );
        let commands = execute
            .getattr("call_args")
            .unwrap()
            .getattr("args")
            .unwrap()
            .get_item(1)
            .unwrap();
        assert_eq!(commands.len().unwrap(), 2);
        let pytest: Vec<String> = commands.get_item(0).unwrap().extract().unwrap();
        let cargo: Vec<String> = commands.get_item(1).unwrap().extract().unwrap();
        assert!(pytest.contains(&"test_probe.py".to_owned()));
        assert!(!pytest.iter().any(|arg| arg.ends_with(".rs")));
        assert_eq!(cargo[0], "cargo");
        assert!(cargo.contains(&"python_contracts_memory_vectors".to_owned()));
        kwargs.set_item("coverage", false).unwrap();
        let passed = verification
            .getattr("run_targeted_tests")
            .unwrap()
            .call((&context, &selection, &check), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&passed.getattr("status").unwrap()), "passed");
    });
}

#[test]
fn candidate_runner_executes_candidate_python_extension_and_forge() {
    let mut case = Case::new();
    let root = runtime_fixture(&case);
    conflicting_ambient(&mut case);
    let runtime_dir = case.mkdir("candidate_runtime");
    Python::attach(|py| {
        let plan = runtime_contract_plan(py, &root);
        let args = PyDict::new(py);
        args.set_item("repo", path(py, &root)).unwrap();
        args.set_item("snapshot", path(py, &root)).unwrap();
        args.set_item("runtime_dir", path(py, &runtime_dir))
            .unwrap();
        let ctx = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&args))
            .unwrap();
        let selection = module(py, "conductor.candidate_review.checks")
            .getattr("TestSelection")
            .unwrap()
            .call1((
                PyTuple::empty(py),
                PyDict::new(py),
                PyTuple::empty(py),
                plan,
            ))
            .unwrap();
        let options = PyDict::new(py);
        options.set_item("coverage", false).unwrap();
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("run_targeted_tests")
            .unwrap()
            .call((&ctx, selection, runtime_check(py)), Some(&options))
            .unwrap();
        assert_eq!(
            text(&result.getattr("status").unwrap()),
            "passed",
            "findings: {} stderr: {}",
            result.getattr("findings").unwrap().repr().unwrap(),
            text(&result.getattr("stderr_tail").unwrap()),
        );
        assert!(runtime_dir
            .join("contract-extension/conductor_native.so")
            .is_file());
        assert!(runtime_dir.join("contract-bin/forge").is_file());
        let output = text(&result.getattr("stdout_tail").unwrap());
        assert!(output.contains("candidate_imports_and_binary_are_used ... ok"));
    });
}

#[test]
fn standalone_runner_executes_selected_source_and_binaries() {
    let mut case = Case::new();
    let root = runtime_fixture(&case);
    conflicting_ambient(&mut case);
    Python::attach(|py| {
        let plan = runtime_contract_plan(py, &root);
        let options = PyDict::new(py);
        options.set_item("contract_plan", plan).unwrap();
        let code = module(py, "conductor.graph_test_select")
            .getattr("run_tests")
            .unwrap()
            .call((path(py, &root), PyList::empty(py)), Some(&options))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        assert_eq!(code, 0);
    });
}

#[test]
fn candidate_runner_uses_candidate_slop_over_conflicting_ambient() {
    let mut case = Case::new();
    let root = slop_runtime_fixture(&case);
    conflicting_ambient(&mut case);
    let runtime_dir = case.mkdir("candidate_slop_runtime");
    Python::attach(|py| {
        let plan = runtime_contract_plan(py, &root);
        let args = PyDict::new(py);
        args.set_item("repo", path(py, &root)).unwrap();
        args.set_item("snapshot", path(py, &root)).unwrap();
        args.set_item("runtime_dir", path(py, &runtime_dir))
            .unwrap();
        let ctx = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&args))
            .unwrap();
        let selection = module(py, "conductor.candidate_review.checks")
            .getattr("TestSelection")
            .unwrap()
            .call1((
                PyTuple::empty(py),
                PyDict::new(py),
                PyTuple::empty(py),
                plan,
            ))
            .unwrap();
        let options = PyDict::new(py);
        options.set_item("coverage", false).unwrap();
        let result = module(py, "conductor.candidate_review.verification")
            .getattr("run_targeted_tests")
            .unwrap()
            .call((&ctx, selection, runtime_check(py)), Some(&options))
            .unwrap();
        assert_eq!(
            text(&result.getattr("status").unwrap()),
            "passed",
            "findings: {} stderr: {}",
            result.getattr("findings").unwrap().repr().unwrap(),
            text(&result.getattr("stderr_tail").unwrap()),
        );
        assert!(runtime_dir
            .join("contract-extension/slop_core.so")
            .is_file());
        assert!(text(&result.getattr("stdout_tail").unwrap())
            .contains("candidate_slop_extension_is_used ... ok"));
    });
}

#[test]
fn standalone_runner_uses_candidate_slop_over_conflicting_ambient() {
    let mut case = Case::new();
    let root = slop_runtime_fixture(&case);
    conflicting_ambient(&mut case);
    Python::attach(|py| {
        let plan = runtime_contract_plan(py, &root);
        let options = PyDict::new(py);
        options.set_item("contract_plan", plan).unwrap();
        let code = module(py, "conductor.graph_test_select")
            .getattr("run_tests")
            .unwrap()
            .call((path(py, &root), PyList::empty(py)), Some(&options))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        assert_eq!(code, 0);
    });
}
