#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::{
    batch_execution,
    cgroup::{Cgroup, Limits as GroupLimits},
    confined_execution::{self, Completion, Limits},
    sandbox::{CommandPlan, Policy, Sandbox},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

struct Job {
    plan: CommandPlan,
    group: Cgroup,
    pid_file: PathBuf,
    notify: tokio::sync::oneshot::Sender<Completion>,
}
fn fixture(host: &support::Host) -> (Job, PathBuf, tokio::sync::oneshot::Receiver<Completion>) {
    let scratch = host.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
    let program = support::task_worker();
    let sandbox = Sandbox::prepare(Policy {
        read_only: vec![
            "/usr".into(),
            "/lib".into(),
            "/lib64".into(),
            "/etc/ld.so.cache".into(),
            program.into(),
        ],
        scratch: scratch.clone(),
        working_directory: scratch.clone(),
        environment: BTreeMap::new(),
    })
    .unwrap();
    let mut plan = sandbox.command(program).unwrap();
    plan.command().args(["pid-sleep", "20000", "child.pid"]);
    let parent = PathBuf::from(
        std::env::var_os("FORGE_TEST_CGROUP_PARENT").expect("delegated parent required"),
    );
    let group = Cgroup::create(
        &parent,
        GroupLimits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        },
    )
    .unwrap();
    let path = group.report().path.clone();
    let (notify, receive) = tokio::sync::oneshot::channel();
    (
        Job {
            plan,
            group,
            pid_file: scratch.join("child.pid"),
            notify,
        },
        path,
        receive,
    )
}
fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(10),
        max_output_chars: 32,
        cpu_seconds: None,
        address_space_bytes: None,
    }
}
async fn wait_pid(path: &Path) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
fn assert_dead(path: &Path) {
    let pid: i32 = fs::read_to_string(path).unwrap().parse().unwrap();
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => assert!(
            stat.rsplit_once(')').unwrap().1.starts_with(" Z "),
            "{stat}"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn panicked_worker_batch_waits_for_registered_process_cleanup() {
    let host = support::Host::new();
    let (job, group_path, receive) = fixture(&host);
    let pid_file = job.pid_file.clone();
    let batch = batch_execution::start(vec![job], 1, |job, _, owners| async move {
        let execution = confined_execution::start(job.plan, job.group, limits())?;
        let _guard = owners.register(&execution)?;
        wait_pid(&job.pid_file).await?;
        job.notify
            .send(execution.completion())
            .map_err(|_| anyhow::anyhow!("observer disappeared"))?;
        panic!("injected panic after child start");
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    })
    .unwrap();
    let mut completion = tokio::time::timeout(Duration::from_secs(4), receive)
        .await
        .unwrap()
        .unwrap();
    let results = tokio::time::timeout(Duration::from_secs(8), batch.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(results[0].is_err());
    assert!(
        completion
            .finished()
            .expect("batch returned before terminal")
            .cleanup_complete
    );
    assert!(completion.wait().await.unwrap().cleanup_complete);
    assert!(!group_path.exists());
    assert_dead(&pid_file);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn dropped_batch_still_cleans_registered_process() {
    let host = support::Host::new();
    let (job, group_path, receive) = fixture(&host);
    let pid_file = job.pid_file.clone();
    let batch = batch_execution::start(vec![job], 1, |job, cancel, owners| async move {
        let execution = confined_execution::start(job.plan, job.group, limits())?;
        let _guard = owners.register(&execution)?;
        wait_pid(&job.pid_file).await?;
        job.notify
            .send(execution.completion())
            .map_err(|_| anyhow::anyhow!("observer disappeared"))?;
        cancel.cancelled().await?;
        Ok::<(), anyhow::Error>(())
    })
    .unwrap();
    let mut completion = tokio::time::timeout(Duration::from_secs(4), receive)
        .await
        .unwrap()
        .unwrap();
    drop(batch);
    let terminal = tokio::time::timeout(Duration::from_secs(8), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(terminal.cleanup_complete);
    assert!(!group_path.exists());
    assert_dead(&pid_file);
}
