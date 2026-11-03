use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture {
    root: PathBuf,
    host: PathBuf,
    config: PathBuf,
    scratch: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-candidate-workspace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let host = root.join("host");
        fs::create_dir(&host).unwrap();
        let repo = git2::Repository::init(&host).unwrap();
        repo.index().unwrap().write().unwrap();
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("grants.json");
        let fixture = Self {
            root,
            host,
            config,
            scratch,
        };
        fixture.write_config(fixture.value());
        fixture
    }
    fn value(&self) -> Value {
        json!({"version":1,"source_root":self.host,"read_only":[self.host,"/usr/bin"],"programs":{"true":"/usr/bin/true"},"environment":{},"scratch_parent":self.scratch,"cgroup_parent":"/sys/fs/cgroup","resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},"max_wall_seconds":60,"max_cpu_seconds":60,"max_address_space_mib":1024,"python":null})
    }
    fn write_config(&self, value: Value) {
        fs::write(&self.config, value.to_string()).unwrap();
        fs::set_permissions(&self.config, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn source(&self) -> Source {
        Source::open(&self.host).unwrap()
    }
    fn tree(&self) -> String {
        git2::Repository::open(&self.host)
            .unwrap()
            .treebuilder(None)
            .unwrap()
            .write()
            .unwrap()
            .to_string()
    }
    fn grants(&self) -> Grants {
        Grants::load(&self.config, &self.source()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn grants_reject_unsafe_or_untrusted_configuration() {
    let f = Fixture::new();
    let source = f.source();
    assert_eq!(f.grants().digest().len(), 64);
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o660)).unwrap();
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["version"] = json!(2);
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["unexpected"] = json!(true);
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["environment"] = json!({"GIT_DIR":"/tmp"});
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    fs::remove_file(&f.config).unwrap();
    symlink(f.host.join("missing"), &f.config).unwrap();
    assert!(Grants::load(&f.config, &source).is_err());
}
#[test]
fn private_git_is_detached_and_source_index_untouched() {
    let f = Fixture::new();
    let source = f.source();
    let tree = f.tree();
    let grants = f.grants();
    let before = fs::read(f.host.join(".git/index")).unwrap();
    let before_head = fs::read(f.host.join(".git/HEAD")).unwrap();
    let workspace = Workspace::new(&source, &tree, &grants).unwrap();
    let repo = git2::Repository::open_bare(&workspace.git_dir).unwrap();
    assert!(repo.head_detached().unwrap());
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree_id()
            .to_string(),
        tree
    );
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), before);
    assert_eq!(fs::read(f.host.join(".git/HEAD")).unwrap(), before_head);
    assert!(repo.find_reference("refs/heads/master").is_err());
    drop(repo);
    let path = workspace.scratch.path.clone();
    workspace.discard().unwrap();
    assert!(!path.exists());
}
#[test]
fn armed_workspace_is_retained_after_unsafe_cleanup() {
    let f = Fixture::new();
    let source = f.source();
    let mut workspace = Workspace::new(&source, &f.tree(), &f.grants()).unwrap();
    let scratch = workspace.scratch.path.clone();
    let snapshot = workspace.path();
    Resources::arm(&mut workspace);
    assert!(Box::new(workspace).finish(false).is_err());
    assert!(scratch.is_dir() && snapshot.is_dir());
    fs::remove_dir_all(scratch).unwrap();
}
#[test]
fn changed_file_artifact_rejects_escape() {
    let f = Fixture::new();
    let source = f.source();
    let grants = f.grants();
    let workspace = Workspace::new(&source, &f.tree(), &grants).unwrap();
    let bad = PreparedArgv {
        argv: vec!["true".into()],
        changed_files: Some(crate::candidate_command::ChangedFiles {
            path: workspace.runtime().join("../escape"),
            bytes: b"x".to_vec(),
        }),
    };
    assert!(workspace.plan(&bad, &grants).is_err());
    assert!(!workspace.scratch.path.join("escape").exists());
    workspace.discard().unwrap();
}

#[test]
fn artifact_owner_captures_only_after_successful_discard() {
    use crate::confined_execution::{FinishState, Outcome, Resources};
    let f = Fixture::new();
    let workspace = Workspace::new(&f.source(), &f.tree(), &f.grants()).unwrap();
    let scratch = workspace.scratch.path.clone();
    fs::write(workspace.runtime().join("coverage.json"), b"{\"files\":{}}").unwrap();
    let (mut owner, mut receiver) = crate::candidate_stage_artifact::Capture::new(
        workspace,
        "coverage.json",
        4096,
        artifact_budget(),
    )
    .unwrap();
    owner.arm();
    Box::new(owner)
        .finish_with_state(FinishState {
            cleanup_safe: true,
            outcome: Outcome::Exited,
            exit_code: Some(0),
        })
        .unwrap();
    assert!(!scratch.exists());
    let artifact = receiver.try_recv().unwrap().unwrap();
    assert_eq!(artifact.bytes, b"{\"files\":{}}");
    assert_eq!(artifact.sha256.len(), 64);
}
#[test]
fn artifact_owner_refuses_missing_symlink_and_uncertain_cleanup() {
    use crate::confined_execution::{FinishState, Outcome, Resources};
    for case in ["missing", "symlink", "uncertain"] {
        let f = Fixture::new();
        let workspace = Workspace::new(&f.source(), &f.tree(), &f.grants()).unwrap();
        let scratch = workspace.scratch.path.clone();
        let path = workspace.runtime().join("coverage.json");
        if case == "symlink" {
            symlink(f.host.join("sentinel"), &path).unwrap();
        }
        if case == "uncertain" {
            fs::write(&path, b"{}\n").unwrap();
        }
        let (mut owner, mut receiver) = crate::candidate_stage_artifact::Capture::new(
            workspace,
            "coverage.json",
            4096,
            artifact_budget(),
        )
        .unwrap();
        owner.arm();
        let result = Box::new(owner).finish_with_state(FinishState {
            cleanup_safe: case != "uncertain",
            outcome: Outcome::Exited,
            exit_code: Some(0),
        });
        assert!(result.is_err());
        assert!(receiver.try_recv().unwrap().is_err());
        if case == "uncertain" {
            assert!(scratch.exists());
            fs::remove_dir_all(scratch).unwrap();
        } else {
            assert!(!scratch.exists());
        }
    }
}
#[test]
fn dropped_artifact_waiter_still_discards_owned_workspace() {
    use crate::confined_execution::{FinishState, Outcome, Resources};
    let f = Fixture::new();
    let workspace = Workspace::new(&f.source(), &f.tree(), &f.grants()).unwrap();
    let scratch = workspace.scratch.path.clone();
    fs::write(workspace.runtime().join("coverage.json"), b"{}").unwrap();
    let (mut owner, receiver) = crate::candidate_stage_artifact::Capture::new(
        workspace,
        "coverage.json",
        4096,
        artifact_budget(),
    )
    .unwrap();
    owner.arm();
    drop(receiver);
    Box::new(owner)
        .finish_with_state(FinishState {
            cleanup_safe: true,
            outcome: Outcome::Exited,
            exit_code: Some(0),
        })
        .unwrap();
    assert!(!scratch.exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and native sandbox support"]
async fn real_owner_timeout_and_dropped_waiter_never_deliver_coverage() {
    use crate::{
        cgroup::Cgroup,
        confined_execution::{self, Limits, Outcome},
    };
    let parent = std::env::var("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation");
    for dropped in [false, true] {
        let f = Fixture::new();
        let mut value = f.value();
        value["cgroup_parent"] = json!(parent);
        value["read_only"] = json!([f.host, "/usr", "/lib", "/lib64", "/etc/ld.so.cache"]);
        value["programs"] = json!({"sleep":"/usr/bin/sleep"});
        f.write_config(value);
        let grants = f.grants();
        let workspace = Workspace::new(&f.source(), &f.tree(), &grants).unwrap();
        let scratch = workspace.scratch.path.clone();
        fs::write(workspace.runtime().join("coverage.json"), b"{}\n").unwrap();
        let command = workspace
            .plan(
                &PreparedArgv {
                    argv: vec!["sleep".into(), "30".into()],
                    changed_files: None,
                },
                &grants,
            )
            .unwrap();
        let (capture, receiver) = crate::candidate_stage_artifact::Capture::new(
            workspace,
            "coverage.json",
            4096,
            artifact_budget(),
        )
        .unwrap();
        let cgroup = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
        let group_path = cgroup.report().path.clone();
        let limits = Limits {
            timeout: std::time::Duration::from_millis(200),
            max_output_chars: 4096,
            cpu_seconds: None,
            address_space_bytes: None,
        };
        let execution =
            confined_execution::start_with_resources(command, cgroup, limits, Box::new(capture))
                .unwrap();
        let mut completion = execution.completion();
        if dropped {
            drop(execution);
        } else {
            let report = tokio::time::timeout(std::time::Duration::from_secs(10), execution.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(report.outcome, Outcome::TimedOut);
            assert!(report.cleanup_complete);
        }
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(10), completion.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(terminal.cleanup_complete);
        assert!(matches!(
            terminal.outcome,
            Outcome::TimedOut | Outcome::Cancelled
        ));
        assert!(receiver.await.unwrap().is_err());
        assert!(!scratch.exists());
        assert!(!group_path.exists());
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT, FORGE_TEST_BIN, FORGE_TEST_PYTHON, FORGE_TEST_PYTEST and FORGE_TEST_COVERAGE"]
async fn real_supervised_python_shard_captures_and_scores_coverage() {
    use crate::{
        candidate_command::PreparedArgv,
        cgroup::Cgroup,
        confined_execution::{self, Limits, Outcome},
    };
    let parent = std::env::var("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation");
    let forge = std::env::var("FORGE_TEST_BIN").expect("explicit forge binary");
    let python = std::env::var("FORGE_TEST_PYTHON").expect("explicit Python environment");
    let pytest = std::env::var("FORGE_TEST_PYTEST").expect("explicit pytest entrypoint");
    let coverage = std::env::var("FORGE_TEST_COVERAGE").expect("explicit coverage entrypoint");
    let f = Fixture::new();
    fs::create_dir(f.host.join("tests")).unwrap();
    fs::write(f.host.join("sample.py"), "def value():\n    return 42\n").unwrap();
    fs::write(
        f.host.join("tests/test_sample.py"),
        "from sample import value\ndef test_value():\n    assert value() == 42\n",
    )
    .unwrap();
    let repository = git2::Repository::open(&f.host).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(std::path::Path::new("sample.py")).unwrap();
    index
        .add_path(std::path::Path::new("tests/test_sample.py"))
        .unwrap();
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let index_before = fs::read(f.host.join(".git/index")).unwrap();
    let source_before = fs::read(f.host.join("sample.py")).unwrap();
    let mut value = f.value();
    value["cgroup_parent"] = json!(parent);
    value["read_only"] = json!([
        f.host,
        "/usr",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        forge,
        std::path::Path::new(&python)
            .parent()
            .unwrap()
            .parent()
            .unwrap(),
        pytest,
        coverage
    ]);
    value["programs"] = json!({"forge":forge,"python":python,"pytest":pytest,"coverage":coverage});
    value["python"] = json!("python");
    value["resources"]["memory_mib"] = json!(1024);
    f.write_config(value);
    let grants = f.grants();
    let workspace = Workspace::new(&f.source(), &tree, &grants).unwrap();
    let scratch = workspace.scratch.path.clone();
    let root = workspace.path();
    let argv = python_test_argv(&root, tree, python, pytest, coverage);
    let command = workspace
        .plan(
            &PreparedArgv {
                argv,
                changed_files: None,
            },
            &grants,
        )
        .unwrap();
    let (capture, receiver) = crate::candidate_stage_artifact::Capture::new(
        workspace,
        "coverage.json",
        4 * 1024 * 1024,
        artifact_budget(),
    )
    .unwrap();
    let cgroup = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
    let cgroup_path = cgroup.report().path.clone();
    let limits = Limits {
        timeout: std::time::Duration::from_secs(45),
        max_output_chars: 12000,
        cpu_seconds: Some(30),
        address_space_bytes: None,
    };
    let execution =
        confined_execution::start_with_resources(command, cgroup, limits, Box::new(capture))
            .unwrap();
    let report = tokio::time::timeout(std::time::Duration::from_secs(60), execution.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, Outcome::Exited, "{report:?}");
    assert_eq!(report.exit_code, Some(0), "{report:?}");
    assert!(report.cleanup_complete);
    let artifact = receiver.await.unwrap().unwrap();
    assert_sample_coverage(artifact.bytes, &root);
    assert!(!scratch.exists());
    assert!(!cgroup_path.exists());
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), index_before);
    assert_eq!(fs::read(f.host.join("sample.py")).unwrap(), source_before);
}
fn python_test_argv(
    root: &Path,
    tree: String,
    python: String,
    pytest: String,
    coverage: String,
) -> Vec<String> {
    vec![
        "forge".into(),
        "candidate".into(),
        "python-test".into(),
        "--repo".into(),
        root.to_str().unwrap().into(),
        "--base".into(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        "--tree".into(),
        tree,
        "--interpreter".into(),
        python,
        "--pytest".into(),
        pytest,
        "--coverage-tool".into(),
        coverage,
        "--test=tests/test_sample.py".into(),
        "--bind=sample.py".into(),
        "--coverage".into(),
        "--execute".into(),
    ]
}
fn assert_sample_coverage(bytes: Vec<u8>, root: &Path) {
    let changed = crate::candidate_python_coverage::ChangedFile {
        path: "sample.py".into(),
        absolute_path: String::new(),
        lines: [1, 2].into_iter().collect(),
        high_risk: false,
    };
    let observed = crate::candidate_python_coverage::evaluate_shards(
        &[(bytes, root.to_str().unwrap().into())],
        &[changed],
        80.,
        80.,
    )
    .unwrap();
    assert!(observed.other_pass);
    assert!(observed.other.measurable > 0);
}
fn artifact_budget() -> std::sync::Arc<crate::candidate_stage_artifact::ArtifactBudget> {
    std::sync::Arc::new(crate::candidate_stage_artifact::ArtifactBudget::new(
        16 * 1024 * 1024,
    ))
}
#[test]
fn owner_capture_enforces_shared_aggregate_limit() {
    use crate::confined_execution::{FinishState, Outcome, Resources};
    let f = Fixture::new();
    let budget = std::sync::Arc::new(crate::candidate_stage_artifact::ArtifactBudget::new(2));
    for index in 0..2 {
        let workspace = Workspace::new(&f.source(), &f.tree(), &f.grants()).unwrap();
        let scratch = workspace.scratch.path.clone();
        fs::write(workspace.runtime().join("coverage.json"), b"{}").unwrap();
        let (mut owner, mut receiver) = crate::candidate_stage_artifact::Capture::new(
            workspace,
            "coverage.json",
            4096,
            budget.clone(),
        )
        .unwrap();
        owner.arm();
        let result = Box::new(owner).finish_with_state(FinishState {
            cleanup_safe: true,
            outcome: Outcome::Exited,
            exit_code: Some(0),
        });
        assert_eq!(result.is_ok(), index == 0);
        assert_eq!(receiver.try_recv().unwrap().is_ok(), index == 0);
        assert!(!scratch.exists());
    }
}
#[path = "candidate_contract_stage_tests.rs"]
mod contract_stage;
#[path = "candidate_rust_coverage_stage_tests.rs"]
mod rust_coverage_stage;

#[path = "candidate_invariant_stage_tests.rs"]
mod invariant_stage;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT, source-matching FORGE_TEST_BIN, Python with the installed forge_test_record plugin and FORGE_TEST_PYTEST/FORGE_TEST_COVERAGE"]
async fn real_pytest_stage_captures_owned_fixture_and_test_windows_only() {
    let parent = std::env::var("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation");
    let forge = std::env::var("FORGE_TEST_BIN").expect("source-matching Forge binary");
    let python = std::env::var("FORGE_TEST_PYTHON").expect("installed recorder Python");
    let pytest = std::env::var("FORGE_TEST_PYTEST").expect("pytest entrypoint");
    let coverage = std::env::var("FORGE_TEST_COVERAGE").expect("coverage entrypoint");
    let f = Fixture::new();
    let sample = RECORDER_SAMPLE;
    let (tree, forbidden, index_before) = prepare_record_fixture(&f);
    let mut value = f.value();
    value["cgroup_parent"] = json!(parent);
    value["read_only"] = json!([
        f.host,
        "/usr",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        forge,
        Path::new(&python).parent().unwrap().parent().unwrap(),
        pytest,
        coverage
    ]);
    value["programs"] = json!({"forge":forge,"python":python,"pytest":pytest,"coverage":coverage});
    value["python"] = json!("python");
    value["resources"]["memory_mib"] = json!(1024);
    f.write_config(value);
    let grants = f.grants();
    let mut records = Vec::new();
    for (test, config) in [
        ("tests/test_sample.py", "tests/pyproject.toml"),
        ("test_root.py", "pyproject.toml"),
    ] {
        records.push(
            capture_record_stage(
                &f,
                &grants,
                &tree,
                &python,
                &pytest,
                &coverage,
                (test, config),
            )
            .await,
        );
    }
    assert_eq!(fs::read(&forbidden).unwrap(), b"host sentinel");
    assert_eq!(
        fs::read(f.host.join("sample.py")).unwrap(),
        sample.as_bytes()
    );
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), index_before);
    assert_record_shard_windows(&f.root, sample, &records);
}

async fn capture_record_stage(
    f: &Fixture,
    grants: &Grants,
    tree: &str,
    python: &str,
    pytest: &str,
    coverage: &str,
    selection: (&str, &str),
) -> Vec<u8> {
    use crate::{
        candidate_command::PreparedArgv,
        cgroup::Cgroup,
        confined_execution::{self, Limits, Outcome},
    };
    let (test, config) = selection;
    let workspace = Workspace::new(&f.source(), tree, grants).unwrap();
    let scratch = workspace.scratch.path.clone();
    let snapshot = workspace.path();
    let mut argv = python_test_argv(
        &snapshot,
        tree.to_owned(),
        python.to_owned(),
        pytest.to_owned(),
        coverage.to_owned(),
    );
    let test_arg = argv
        .iter_mut()
        .find(|arg| arg.starts_with("--test="))
        .unwrap();
    *test_arg = format!("--test={test}");
    argv.push("--record-owned".into());
    argv.push("--record-config".into());
    argv.push(config.into());
    argv.push(format!("--bind={config}"));
    let command = workspace
        .plan(
            &PreparedArgv {
                argv,
                changed_files: None,
            },
            grants,
        )
        .unwrap();
    let budget = artifact_budget();
    let (capture, receiver) = crate::candidate_stage_artifact::CapturePair::new(
        workspace,
        crate::candidate_stage_artifact::Request {
            name: "coverage.json",
            maximum: 4 * 1024 * 1024,
            budget: budget.clone(),
        },
        crate::candidate_stage_artifact::Request {
            name: "test-record.sqlite",
            maximum: 4 * 1024 * 1024,
            budget,
        },
    )
    .unwrap();
    let cgroup = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
    let cgroup_path = cgroup.report().path.clone();
    let limits = Limits {
        timeout: std::time::Duration::from_secs(45),
        max_output_chars: 12000,
        cpu_seconds: Some(30),
        address_space_bytes: None,
    };
    let execution =
        confined_execution::start_with_resources(command, cgroup, limits, Box::new(capture))
            .unwrap();
    let report = tokio::time::timeout(std::time::Duration::from_secs(60), execution.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, Outcome::Exited, "{report:?}");
    assert_eq!(report.exit_code, Some(0), "{report:?}");
    assert!(report.cleanup_complete);
    let mut artifacts = receiver.await.unwrap().unwrap();
    assert_eq!(artifacts.len(), 2);
    let record = artifacts.pop().unwrap().bytes;
    let _coverage = artifacts.pop().unwrap();
    assert!(!scratch.exists() && !cgroup_path.exists());
    record
}

const RECORDER_SAMPLE: &str = "def import_only():\n    return 1\n\ndef fixture_hit():\n    return 2\n\ndef test_hit():\n    return 3\n\ndef missed():\n    return 4\n\nimport_only()\n";

fn prepare_record_fixture(f: &Fixture) -> (String, PathBuf, Vec<u8>) {
    fs::create_dir(f.host.join("tests")).unwrap();
    fs::write(f.host.join("sample.py"), RECORDER_SAMPLE).unwrap();
    fs::write(f.host.join("tests/test_sample.py"), "import pytest\nimport sample\nfrom pathlib import Path\n\n@pytest.fixture\ndef checked():\n    assert sample.fixture_hit() == 2\n\ndef test_value(checked, request):\n    assert request.config.rootpath == Path(__file__).parent\n    assert checked is None\n").unwrap();
    fs::write(f.host.join("tests/pyproject.toml"), "[tool.pytest.ini_options]\naddopts = ''\n[tool.forge.test_record]\nstore = 'nested.sqlite'\nlines = true\n").unwrap();
    fs::write(
        f.host.join("test_root.py"),
        "import sample\ndef test_root():\n    assert sample.test_hit() == 3\n",
    )
    .unwrap();
    let forbidden = f.root.join("host-record.sqlite");
    fs::write(&forbidden, b"host sentinel").unwrap();
    fs::write(
        f.host.join("pyproject.toml"),
        format!(
            "[tool.forge.test_record]\nstore = {:?}\nlines = true\n",
            forbidden.to_str().unwrap()
        ),
    )
    .unwrap();
    let repository = git2::Repository::open(&f.host).unwrap();
    let mut index = repository.index().unwrap();
    for path in [
        "sample.py",
        "test_root.py",
        "tests/test_sample.py",
        "tests/pyproject.toml",
        "pyproject.toml",
    ] {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let index_before = fs::read(f.host.join(".git/index")).unwrap();
    (tree, forbidden, index_before)
}

fn assert_record_shard_windows(root: &Path, sample: &str, records: &[Vec<u8>]) {
    let analysis = root.join("analysis");
    fs::create_dir(&analysis).unwrap();
    fs::write(analysis.join("sample.py"), sample).unwrap();
    let changed = [("sample.py".to_owned(), vec![2, 5, 8, 11])];
    let mut missing = None;
    for (index, record) in records.iter().enumerate() {
        let path = analysis.join(format!("record-{index}.sqlite"));
        fs::write(&path, record).unwrap();
        let (count, rows) =
            conductor_native::test_record_untested::untested_owned(&analysis, &path, &changed)
                .unwrap();
        assert_eq!(count, 4);
        missing = Some(crate::candidate_targeted_python::intersect_missing(
            missing,
            rows.into_iter().collect(),
        ));
    }
    let missing = missing.unwrap().into_iter().collect::<Vec<_>>();
    assert_eq!(
        missing,
        vec![
            ("sample.py".into(), "import_only".into(), 1),
            ("sample.py".into(), "missed".into(), 10)
        ]
    );
    fs::write(
        analysis.join("sample.py"),
        sample.replace("return 3", "return 5"),
    )
    .unwrap();
    for index in 0..records.len() {
        let path = analysis.join(format!("record-{index}.sqlite"));
        let (count, stale) =
            conductor_native::test_record_untested::untested_owned(&analysis, &path, &changed)
                .unwrap();
        assert_eq!(
            (count, stale.len()),
            (4, 4),
            "source drift must reject all windows"
        );
    }
}

#[test]
fn paired_capture_propagates_workspace_cleanup_failure_after_failed_shard() {
    use crate::confined_execution::{FinishState, Outcome, Resources};
    let f = Fixture::new();
    let workspace = Workspace::new(&f.source(), &f.tree(), &f.grants()).unwrap();
    let scratch = workspace.scratch.path.clone();
    let budget = artifact_budget();
    let (mut owner, mut receiver) = crate::candidate_stage_artifact::CapturePair::new(
        workspace,
        crate::candidate_stage_artifact::Request {
            name: "coverage.json",
            maximum: 4096,
            budget: budget.clone(),
        },
        crate::candidate_stage_artifact::Request {
            name: "record.sqlite",
            maximum: 4096,
            budget,
        },
    )
    .unwrap();
    owner.arm();
    fs::rename(&scratch, f.root.join("moved-scratch")).unwrap();
    let result = Box::new(owner).finish_with_state(FinishState {
        cleanup_safe: true,
        outcome: Outcome::Exited,
        exit_code: Some(1),
    });
    assert!(
        result.is_err(),
        "workspace cleanup failure must propagate even for a failed shard"
    );
    assert!(receiver.try_recv().unwrap().is_err());
}

#[test]
fn separate_stage_workspaces_do_not_share_candidate_writes() {
    let f = Fixture::new();
    let source = f.source();
    let tree = f.tree();
    let grants = f.grants();
    let review = Workspace::new(&source, &tree, &grants).unwrap();
    let probe = Workspace::new(&source, &tree, &grants).unwrap();
    fs::write(probe.path().join("side_effect"), "probe output").unwrap();
    assert!(!review.path().join("side_effect").exists());
    assert!(!f.host.join("side_effect").exists());
    let probe_scratch = probe.scratch.path.clone();
    let review_scratch = review.scratch.path.clone();
    probe.discard().unwrap();
    review.discard().unwrap();
    assert!(!probe_scratch.exists());
    assert!(!review_scratch.exists());
}
