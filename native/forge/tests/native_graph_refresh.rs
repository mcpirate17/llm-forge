#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::{Command, Output};

fn command(host: &support::Host) -> Command {
    let mut cmd = host.command();
    cmd.env("PATH", "")
        .env_remove("CRG_DATA_DIR")
        .env_remove("FORGE_NATIVE_HOOKS");
    cmd
}
fn refresh(host: &support::Host) -> Output {
    command(host)
        .args(["graph", "--host"])
        .arg(&host.0)
        .arg("refresh")
        .output()
        .unwrap()
}
fn wait(host: &support::Host) -> Output {
    command(host)
        .args(["graph", "--host"])
        .arg(&host.0)
        .args(["wait", "--timeout-seconds", "10"])
        .output()
        .unwrap()
}
fn okay(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fixture() -> support::Host {
    let host = support::Host::new();
    // A valid unborn repository exercises native libgit2 without a git process.
    git2::Repository::init(&host.0).unwrap();
    fs::write(
        host.0.join("main.rs"),
        "fn first() {}\nfn caller() { first(); }\n",
    )
    .unwrap();
    host
}
fn names(host: &support::Host) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        host.0.join(".forge/graph.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = db.prepare("SELECT name FROM nodes ORDER BY name").unwrap();
    query
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn refresh_runs_native_worker_with_empty_path_and_handles_changed_and_deleted_sources() {
    let host = fixture();
    assert_eq!(okay(refresh(&host))["queued"], true);
    assert_eq!(okay(wait(&host))["fresh"], true);
    assert!(names(&host).contains(&"first".into()));
    fs::write(host.0.join("main.rs"), "fn replacement() {}\n").unwrap();
    okay(refresh(&host));
    okay(wait(&host));
    let result = names(&host);
    assert!(result.contains(&"replacement".into()));
    assert!(!result.contains(&"first".into()));
    fs::remove_file(host.0.join("main.rs")).unwrap();
    okay(refresh(&host));
    okay(wait(&host));
    assert!(names(&host).is_empty());
    assert!(!host.0.join(".code-review-graph/refresh.inflight").exists());
}

#[test]
fn failed_batch_preserves_previous_graph_and_stale_status_until_successful_retry() {
    let host = fixture();
    okay(refresh(&host));
    okay(wait(&host));
    let graph = fs::read(host.0.join(".forge/graph.db")).unwrap();
    fs::write(host.0.join("main.rs"), "fn broken( {").unwrap();
    okay(refresh(&host));
    let failure = wait(&host);
    assert!(!failure.status.success());
    let report: Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(report["fresh"], false);
    assert!(report["stale"].as_str().unwrap().contains("parse"));
    assert_eq!(fs::read(host.0.join(".forge/graph.db")).unwrap(), graph);
    let repeated = wait(&host);
    assert!(!repeated.status.success());
    // A valid unrelated source must not make callers/dependents from the old
    // generation look current after another source failed to refresh.
    fs::write(host.0.join("other.rs"), "fn unrelated() {}\n").unwrap();
    let blocked = command(&host)
        .args(["graph", "--host"])
        .arg(&host.0)
        .args(["context", "other.rs"])
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("stale"));
    fs::write(host.0.join("main.rs"), "fn recovered() {}\n").unwrap();
    okay(refresh(&host));
    okay(wait(&host));
    assert!(names(&host).contains(&"recovered".into()));
    assert!(!host.0.join(".code-review-graph/refresh.stale").exists());
}

#[test]
fn concurrent_requests_coalesce_and_orphaned_inflight_work_recovers() {
    let host = fixture();
    let mut children = Vec::new();
    for _ in 0..24 {
        children.push(
            command(&host)
                .args(["graph", "--host"])
                .arg(&host.0)
                .arg("refresh")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    okay(wait(&host));
    assert!(names(&host).contains(&"first".into()));
    fs::write(host.0.join("main.rs"), "fn after_crash() {}\n").unwrap();
    fs::write(
        host.0.join(".code-review-graph/refresh.inflight"),
        "main.rs\n",
    )
    .unwrap();
    okay(wait(&host));
    assert!(names(&host).contains(&"after_crash".into()));
}

#[test]
fn queued_marker_is_durable_and_worker_lock_is_not_stolen() {
    let host = fixture();
    let store = host.0.join(".code-review-graph");
    fs::create_dir(&store).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = fs::File::create(store.join("refresh.lock")).unwrap();
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    for _ in 0..3 {
        okay(refresh(&host));
    }
    assert_eq!(
        fs::read_to_string(store.join("refresh.pending")).unwrap(),
        "*\n*\n*\n"
    );
    assert!(!host.0.join(".forge/graph.db").exists());
    drop(lock);
    okay(wait(&host));
    assert!(names(&host).contains(&"first".into()));
}

#[test]
fn unsafe_queue_files_and_invalid_inherited_descriptors_fail_closed() {
    let host = fixture();
    let store = host.0.join(".code-review-graph");
    fs::create_dir(&store).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
    let outside = host.0.join("outside");
    fs::write(&outside, "retain").unwrap();
    symlink(&outside, store.join("refresh.pending")).unwrap();
    assert!(!refresh(&host).status.success());
    assert_eq!(fs::read_to_string(outside).unwrap(), "retain");
    fs::remove_file(store.join("refresh.pending")).unwrap();
    fs::set_permissions(&store, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(!refresh(&host).status.success());
    fs::set_permissions(&store, fs::Permissions::from_mode(0o700)).unwrap();
    let output = command(&host)
        .args(["graph-refresh-worker", "--host"])
        .arg(&host.0)
        .args(["--lock-fd", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    for missing_fd in ["3", "12345"] {
        let missing = command(&host)
            .args(["graph-refresh-worker", "--host"])
            .arg(&host.0)
            .args(["--lock-fd", missing_fd])
            .output()
            .unwrap();
        assert_eq!(missing.status.code(), Some(2));
    }
}

#[test]
fn relative_state_override_resolves_before_the_worker_changes_directory() {
    let host = fixture();
    let invoking = support::Host::new();
    let output = command(&host)
        .current_dir(&invoking.0)
        .env("CRG_DATA_DIR", "relative-state")
        .args(["graph", "--host"])
        .arg(&host.0)
        .arg("refresh")
        .output()
        .unwrap();
    okay(output);
    let output = command(&host)
        .current_dir(&invoking.0)
        .env("CRG_DATA_DIR", "relative-state")
        .args(["graph", "--host"])
        .arg(&host.0)
        .args(["wait", "--timeout-seconds", "10"])
        .output()
        .unwrap();
    okay(output);
    assert!(names(&host).contains(&"first".into()));
    assert!(invoking.0.join("relative-state/refresh.lock").exists());
    assert!(!host.0.join("relative-state").exists());
}
