use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};

struct Fixture {
    root: PathBuf,
    host: PathBuf,
    config: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::native_id::new("isolated-task-test").unwrap());
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let host = root.join("host");
        fs::create_dir(&host).unwrap();
        let repo = git2::Repository::init(&host).unwrap();
        repo.index().unwrap().write().unwrap();
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("grants.json");
        let value = serde_json::json!({"version":1,"source_root":host,"read_only":[host,"/usr","/lib","/lib64","/etc/ld.so.cache"],"programs":{"true":"/usr/bin/true","false":"/usr/bin/false"},"environment":{},"scratch_parent":scratch,"cgroup_parent":"/sys/fs/cgroup","resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},"max_wall_seconds":60,"max_cpu_seconds":60,"max_address_space_mib":1024,"python":null});
        fs::write(&config, value.to_string()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self { root, host, config }
    }
    fn prepared(&self, artifacts: Vec<String>) -> Prepared {
        let source = Source::open(&self.host).unwrap();
        let repo = git2::Repository::open(&self.host).unwrap();
        let tree = repo.treebuilder(None).unwrap().write().unwrap().to_string();
        let grants = Grants::load(&self.config, &source).unwrap();
        Prepared::new(&source, &tree, &grants, vec!["true".into()], artifacts).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn resource(prepared: Prepared) -> (TaskResource, PathBuf, PathBuf, Arc<Mutex<Option<Receipt>>>) {
    let Prepared {
        workspace,
        plan,
        artifacts,
        tree,
        receipt_path,
        grants_digest,
        source_root,
    } = prepared;
    drop(plan);
    let path = workspace.path();
    let shared = Arc::new(Mutex::new(None));
    let receipt_parent = workspace.scratch.parent.try_clone().unwrap();
    let resource = TaskResource {
        workspace: Some(workspace),
        artifacts,
        tree,
        receipt_path: receipt_path.clone(),
        receipt_parent,
        grants_digest,
        source_root,
        bounds: ArtifactBounds::default(),
        shared: shared.clone(),
    };
    (resource, path, receipt_path, shared)
}
fn state(outcome: confined_execution::Outcome, code: Option<i32>, safe: bool) -> FinishState {
    FinishState {
        outcome,
        exit_code: code,
        cleanup_safe: safe,
    }
}
#[test]
fn admitted_inputs_are_bounded_canonical_and_unique() {
    for paths in [
        vec!["../x".into()],
        vec!["a//b".into()],
        vec!["a".into(), "a".into()],
        vec!["a\nx".into()],
    ] {
        assert!(validate_input(&["true".into()], &paths).is_err());
    }
    assert!(validate_input(&["true".into()], &["docs/result.bin".into()]).is_ok());
    assert!(validate_input(&[], &[]).is_err());
}
#[test]
fn success_retains_hashed_outputs_and_failure_discards_only_private_workspace() {
    let fixture = Fixture::new();
    let original_index = fs::read(fixture.host.join(".git/index")).unwrap();
    fs::write(fixture.host.join("user.txt"), b"untouched").unwrap();
    let (mut task, path, receipt_path, shared) = resource(fixture.prepared(vec!["out.bin".into()]));
    fs::write(path.join("out.bin"), b"result").unwrap();
    task.arm();
    Box::new(task)
        .finish_with_state(state(confined_execution::Outcome::Exited, Some(0), true))
        .unwrap();
    let receipt = shared.lock().unwrap().clone().unwrap();
    assert_eq!(receipt.status, "retained_for_review");
    assert_eq!(receipt.workspace.as_ref(), Some(&path));
    assert_eq!(
        receipt.artifacts[0].sha256,
        format!("{:x}", Sha256::digest(b"result"))
    );
    assert!(receipt_path.exists() && path.join("out.bin").exists());
    let (mut failed, failed_path, failed_receipt, shared) = resource(fixture.prepared(vec![]));
    fs::write(failed_path.join("x"), b"dirty").unwrap();
    failed.arm();
    Box::new(failed)
        .finish_with_state(state(confined_execution::Outcome::Cancelled, None, true))
        .unwrap();
    assert!(!failed_path.exists());
    assert_eq!(
        shared.lock().unwrap().as_ref().unwrap().status,
        "rolled_back_by_discard"
    );
    assert!(failed_receipt.exists());
    assert_eq!(
        fs::read(fixture.host.join("user.txt")).unwrap(),
        b"untouched"
    );
    assert_eq!(
        fs::read(fixture.host.join(".git/index")).unwrap(),
        original_index
    );
}
#[test]
fn unsafe_cleanup_and_symlink_artifact_retain_recovery_without_success() {
    let fixture = Fixture::new();
    let (mut unsafe_task, path, _, shared) = resource(fixture.prepared(vec![]));
    unsafe_task.arm();
    assert!(Box::new(unsafe_task)
        .finish_with_state(state(confined_execution::Outcome::Failed, None, false))
        .is_err());
    assert!(path.is_dir());
    assert_eq!(
        shared.lock().unwrap().as_ref().unwrap().status,
        "incomplete"
    );
    let (mut task, path, _, shared) = resource(fixture.prepared(vec!["out".into()]));
    let target = fixture.root.join("sentinel");
    fs::write(&target, b"safe").unwrap();
    symlink(&target, path.join("out")).unwrap();
    task.arm();
    assert!(Box::new(task)
        .finish_with_state(state(confined_execution::Outcome::Exited, Some(0), true))
        .is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read(&target).unwrap(), b"safe");
    assert_eq!(
        shared.lock().unwrap().as_ref().unwrap().status,
        "incomplete"
    );
}

#[test]
fn artifact_size_limits_fail_closed_and_preserve_recovery() {
    let fixture = Fixture::new();
    for (first, second, per_file, total, accepted) in [
        (b"1234".as_slice(), b"5678".as_slice(), 4, 8, true),
        (b"12345".as_slice(), b"".as_slice(), 4, 8, false),
        (b"1234".as_slice(), b"5678".as_slice(), 4, 7, false),
    ] {
        let (mut task, path, marker, shared) =
            resource(fixture.prepared(vec!["a".into(), "b".into()]));
        fs::write(path.join("a"), first).unwrap();
        fs::write(path.join("b"), second).unwrap();
        task.bounds = ArtifactBounds { per_file, total };
        task.arm();
        let result = Box::new(task).finish_with_state(state(
            confined_execution::Outcome::Exited,
            Some(0),
            true,
        ));
        assert_eq!(result.is_ok(), accepted);
        let receipt = shared.lock().unwrap().clone().unwrap();
        assert_eq!(
            receipt.status,
            if accepted {
                "retained_for_review"
            } else {
                "incomplete"
            }
        );
        assert_eq!(receipt.workspace.as_ref(), Some(&path));
        assert!(path.is_dir() && marker.is_file());
        if !accepted {
            assert!(receipt.cleanup_error.is_some());
        }
    }
}

#[test]
fn production_per_file_limit_rejects_sparse_file_before_read() {
    let fixture = Fixture::new();
    let (mut task, path, marker, shared) = resource(fixture.prepared(vec!["large".into()]));
    fs::File::create(path.join("large"))
        .unwrap()
        .set_len((MAX_ARTIFACT_BYTES + 1) as u64)
        .unwrap();
    task.arm();
    assert!(Box::new(task)
        .finish_with_state(state(confined_execution::Outcome::Exited, Some(0), true))
        .is_err());
    assert_eq!(
        shared.lock().unwrap().as_ref().unwrap().status,
        "incomplete"
    );
    assert!(path.join("large").exists() && marker.is_file());
}

#[test]
fn receipt_collision_preserves_foreign_file_and_retained_workspace() {
    let fixture = Fixture::new();
    let (mut task, path, marker, shared) = resource(fixture.prepared(vec!["output".into()]));
    fs::write(path.join("output"), b"result").unwrap();
    fs::write(&marker, b"foreign receipt").unwrap();
    task.arm();
    assert!(Box::new(task)
        .finish_with_state(state(confined_execution::Outcome::Exited, Some(0), true))
        .is_err());
    let receipt = shared.lock().unwrap().clone().unwrap();
    assert_eq!(receipt.status, "incomplete");
    assert_eq!(receipt.workspace.as_ref(), Some(&path));
    assert_eq!(fs::read(path.join("output")).unwrap(), b"result");
    assert_eq!(fs::read(marker).unwrap(), b"foreign receipt");
    assert!(fs::read_dir(fixture.root.join("scratch"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("forge-task-result")));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn actual_stage_retains_success_and_discards_nonzero_exit() {
    let fixture = Fixture::new();
    let delegated =
        std::env::var_os("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation required");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
    config["cgroup_parent"] = serde_json::to_value(PathBuf::from(delegated)).unwrap();
    fs::write(&fixture.config, serde_json::to_vec(&config).unwrap()).unwrap();
    let source = Source::open(&fixture.host).unwrap();
    let repo = git2::Repository::open(&fixture.host).unwrap();
    let tree = repo.treebuilder(None).unwrap().write().unwrap().to_string();
    let grants = Grants::load(&fixture.config, &source).unwrap();
    for (command, expected) in [
        ("true", "retained_for_review"),
        ("false", "rolled_back_by_discard"),
    ] {
        let prepared =
            Prepared::new(&source, &tree, &grants, vec![command.into()], vec![]).unwrap();
        let path = prepared.workspace_path();
        let marker = prepared.receipt_path().to_owned();
        let task = prepared.start(&grants, None).unwrap();
        let (report, receipt) = tokio::time::timeout(Duration::from_secs(15), task.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(report.cleanup_complete, "{:?}", report.errors);
        assert_eq!(receipt.status, expected);
        assert_eq!(path.exists(), command == "true");
        assert!(marker.is_file());
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
async fn dropped_task_owner_records_discard_without_waiter() {
    let fixture = Fixture::new();
    let index_before = fs::read(fixture.host.join(".git/index")).unwrap();
    fs::write(fixture.host.join("user.txt"), b"user bytes").unwrap();
    let delegated =
        std::env::var_os("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation required");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.config).unwrap()).unwrap();
    config["cgroup_parent"] = serde_json::to_value(PathBuf::from(delegated)).unwrap();
    let worker = PathBuf::from(
        std::env::var_os("FORGE_TEST_TASK_WORKER")
            .expect("explicit prebuilt native worker required"),
    )
    .canonicalize()
    .unwrap();
    config["programs"]["worker"] = serde_json::to_value(&worker).unwrap();
    config["read_only"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(worker.parent().unwrap()).unwrap());
    fs::write(&fixture.config, serde_json::to_vec(&config).unwrap()).unwrap();
    let source = Source::open(&fixture.host).unwrap();
    let repo = git2::Repository::open(&fixture.host).unwrap();
    let tree = repo.treebuilder(None).unwrap().write().unwrap().to_string();
    let grants = Grants::load(&fixture.config, &source).unwrap();
    let prepared = Prepared::new(
        &source,
        &tree,
        &grants,
        vec![
            "worker".into(),
            "pid-sleep".into(),
            "20000".into(),
            "pid.txt".into(),
        ],
        vec![],
    )
    .unwrap();
    let marker = prepared.receipt_path().to_owned();
    let snapshot = prepared.workspace_path();
    let task = prepared.start(&grants, None).unwrap();
    let pid_file = snapshot.join("pid.txt");
    let pid: u32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(raw) = fs::read_to_string(&pid_file) {
                if let Ok(pid) = raw.parse() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(task);
    tokio::time::timeout(Duration::from_secs(15), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
    assert_eq!(receipt["status"], "rolled_back_by_discard");
    assert!(!snapshot.exists());
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(
        fs::read(fixture.host.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        fs::read(fixture.host.join("user.txt")).unwrap(),
        b"user bytes"
    );
}
