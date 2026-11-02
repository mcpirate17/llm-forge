use super::*;
use crate::{
    candidate_command::PreparedArgv,
    cgroup::Cgroup,
    confined_execution::{self, Limits, Outcome},
};
use sha2::{Digest, Sha256};
use std::time::Duration;

struct Tools {
    forge: String,
    python: String,
    pytest: String,
    coverage: String,
    pin: String,
    probe: Vec<u8>,
    probe_path: PathBuf,
}
impl Tools {
    fn load() -> Self {
        let get = |key| std::env::var(key).unwrap_or_else(|_| panic!("missing explicit {key}"));
        let probe_path = PathBuf::from(get("FORGE_TEST_INVARIANT_PROBE"));
        let probe = fs::read(&probe_path).unwrap();
        let expected = get("FORGE_TEST_INVARIANT_PROBE_SHA256");
        assert_eq!(format!("{:x}", Sha256::digest(&probe)), expected);
        Self {
            forge: get("FORGE_TEST_BIN"),
            python: get("FORGE_TEST_PYTHON"),
            pytest: get("FORGE_TEST_PYTEST"),
            coverage: get("FORGE_TEST_COVERAGE"),
            pin: get("FORGE_TEST_TORCH_PIN"),
            probe,
            probe_path,
        }
    }
}
fn setup(f: &Fixture, tools: &Tools, proc_grant: bool) -> (String, Grants) {
    fs::create_dir(f.host.join("tests")).unwrap();
    fs::write(
        f.host.join("tests/test_invariant.py"),
        "import torch\ndef test_external():\n    from captured_prod import value\n    assert value() == 42 and torch.__version__\n",
    )
    .unwrap();
    fs::write(
        f.host.join("captured_prod.py"),
        "def value():\n    return 42\n",
    )
    .unwrap();
    let repository = git2::Repository::open(&f.host).unwrap();
    let mut index = repository.index().unwrap();
    index
        .add_path(Path::new("tests/test_invariant.py"))
        .unwrap();
    index.add_path(Path::new("captured_prod.py")).unwrap();
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let mut value = f.value();
    value["cgroup_parent"] = json!(std::env::var("FORGE_TEST_CGROUP_PARENT").unwrap());
    let mut reads = vec![
        f.host.clone(),
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/etc/ld.so.cache"),
        PathBuf::from(&tools.forge),
        Path::new(&tools.python)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned(),
        PathBuf::from(&tools.pytest),
        PathBuf::from(&tools.coverage),
        tools.probe_path.parent().unwrap().to_owned(),
    ];
    if proc_grant {
        reads.push(PathBuf::from("/proc"));
    }
    value["read_only"] = json!(reads);
    value["programs"] = json!({"forge":tools.forge,"python":tools.python,"pytest":tools.pytest,"coverage":tools.coverage});
    value["python"] = json!("python");
    value["environment"]["PYTEST_DISABLE_PLUGIN_AUTOLOAD"] = json!("1");
    value["resources"]["memory_mib"] = json!(4096);
    value["resources"]["processes"] = json!(64);
    f.write_config(value);
    (tree, f.grants())
}
fn arguments(f: &Fixture, root: &Path, tree: &str, tools: &Tools, phase: &str) -> Vec<String> {
    vec![
        "forge".into(),
        "candidate".into(),
        "mutation-invariant".into(),
        "--repo".into(),
        root.to_str().unwrap().into(),
        "--source-root".into(),
        f.host.to_str().unwrap().into(),
        "--base".into(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        "--tree".into(),
        tree.into(),
        "--python".into(),
        tools.python.clone(),
        "--pytest".into(),
        tools.pytest.clone(),
        "--coverage".into(),
        tools.coverage.clone(),
        "--nodeid=tests/test_invariant.py::test_external".into(),
        format!("--torch-pin={}", tools.pin),
        "--phase".into(),
        phase.into(),
        "--probe-sha256".into(),
        format!("{:x}", Sha256::digest(&tools.probe)),
        "--probe-path".into(),
        tools.probe_path.to_str().unwrap().into(),
        "--execute".into(),
    ]
}
async fn run(
    f: &Fixture,
    tree: &str,
    grants: &Grants,
    tools: &Tools,
    phase: &str,
) -> (
    confined_execution::Report,
    Option<crate::candidate_stage_artifact::Artifact>,
) {
    let workspace = Workspace::new(&f.source(), tree, grants).unwrap();
    let scratch = workspace.scratch.path.clone();
    let argv = arguments(f, &workspace.path(), tree, tools, phase);
    let plan = workspace
        .plan(
            &PreparedArgv {
                argv,
                changed_files: None,
            },
            grants,
        )
        .unwrap();
    let (capture, receiver) = crate::candidate_stage_artifact::Capture::new(
        workspace,
        "invariant.json",
        4 * 1024 * 1024,
        artifact_budget(),
    )
    .unwrap();
    let group = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
    let group_path = group.report().path.clone();
    let limits = Limits {
        timeout: Duration::from_secs(90),
        max_output_chars: 12000,
        cpu_seconds: Some(60),
        address_space_bytes: None,
    };
    let execution =
        confined_execution::start_with_resources(plan, group, limits, Box::new(capture)).unwrap();
    let report = tokio::time::timeout(Duration::from_secs(120), execution.wait())
        .await
        .unwrap()
        .unwrap();
    let artifact = receiver.await.unwrap().ok();
    assert!(report.cleanup_complete, "{report:?}");
    assert!(report.resources.verified, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(!scratch.exists());
    assert!(!group_path.exists());
    (report, artifact)
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicit delegated cgroup, Forge, PyTorch environment and native probe"]
async fn real_invariant_stages_require_proc_and_capture_owned_evidence() {
    let tools = Tools::load();
    let f = Fixture::new();
    let (tree, grants) = setup(&f, &tools, true);
    let index_before = fs::read(f.host.join(".git/index")).unwrap();
    let source_before = fs::read(f.host.join("tests/test_invariant.py")).unwrap();
    for phase in ["baseline", "node"] {
        let (report, artifact) = run(&f, &tree, &grants, &tools, phase).await;
        assert_eq!(report.outcome, Outcome::Exited, "{report:?}");
        assert_eq!(report.exit_code, Some(0), "{report:?}");
        let value: Value = serde_json::from_slice(&artifact.unwrap().bytes).unwrap();
        assert_eq!(value["phase"], phase);
        assert_eq!(value["torch_version"], tools.pin);
        let files = value["coverage"]["files"].as_object().unwrap();
        let observed = files
            .iter()
            .find(|(path, _)| path.ends_with("captured_prod.py"));
        if phase == "baseline" {
            assert!(observed.is_none());
        } else {
            assert!(observed.unwrap().1["executed_lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == 2));
        }
    }
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), index_before);
    assert_eq!(
        fs::read(f.host.join("tests/test_invariant.py")).unwrap(),
        source_before
    );
    let missing = Fixture::new();
    let (tree, grants) = setup(&missing, &tools, false);
    let (report, artifact) = run(&missing, &tree, &grants, &tools, "node").await;
    assert_ne!(report.exit_code, Some(0), "missing /proc must refuse");
    assert!(artifact.is_none());
    let shadow = Fixture::new();
    let (_, grants) = setup(&shadow, &tools, true);
    let fake = format!(
        "__version__ = {}\n",
        serde_json::to_string(&tools.pin).unwrap()
    );
    fs::write(shadow.host.join("torch.py"), fake).unwrap();
    let repository = git2::Repository::open(&shadow.host).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("torch.py")).unwrap();
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let (report, artifact) = run(&shadow, &tree, &grants, &tools, "node").await;
    assert_ne!(
        report.exit_code,
        Some(0),
        "same-version torch shadow must refuse"
    );
    assert!(artifact.is_none());
}
