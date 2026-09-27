//! Real CLI and storage checks for durable task execution and read-only health.

#[path = "../src/task_store.rs"]
mod task_store;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
use std::time::Instant;
use task_store::{Completion, State, Store, Task};

struct Host(PathBuf);
impl Host {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-tasks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".git")).unwrap();
        Self(path)
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(args)
            .current_dir(&self.0)
            .env_remove("CONDUCTOR_HOST_ROOT")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env("LEDGER_ROOT", self.0.join("ledger"))
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let result = self.cli(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn example(id: &str, now: i64) -> Task {
    Task {
        id: id.into(),
        title: "test task".into(),
        state: State::Assigned,
        owner: Some("agent".into()),
        command: Vec::new(),
        depends_on: Vec::new(),
        claim_id: None,
        session_id: None,
        message_id: None,
        attempt: 0,
        max_attempts: 3,
        lease_until: None,
        created_at: now,
        updated_at: now,
        exit_code: None,
        detail: None,
        receipt: None,
    }
}

fn success() -> Completion {
    Completion {
        state: State::Succeeded,
        exit_code: Some(0),
        detail: None,
        receipt: None,
    }
}

#[test]
fn dependency_leases_and_attempt_fences_survive_reopening() {
    let host = Host::new();
    let mut store = Store::open(&host.0, true).unwrap().unwrap();
    store.create(example("first", 100), "operator").unwrap();
    let mut next = example("next", 100);
    next.depends_on.push("first".into());
    store.create(next, "operator").unwrap();
    assert!(store.start("next", "agent", 10, 100).is_err());
    assert!(store.start("first", "other", 10, 100).is_err());
    let first = store.start("first", "agent", 10, 100).unwrap();
    assert_eq!(first.attempt, 1);
    assert!(store.retry("first", "agent", 109).is_err());
    assert!(store.heartbeat("first", "agent", 1, 10, 110).is_err());
    assert_eq!(store.expired_count(110).unwrap(), 1);
    store.retry("first", "agent", 110).unwrap();
    store.start("first", "agent", 10, 110).unwrap();
    assert!(store.finish("first", "agent", 1, success(), 111).is_err());
    store.heartbeat("first", "agent", 2, 20, 115).unwrap();
    store.finish("first", "agent", 2, success(), 130).unwrap();
    store.start("next", "agent", 10, 130).unwrap();
    drop(store);
    let store = task_store::required(&host.0).unwrap();
    assert_eq!(store.get("first").unwrap().state, State::Succeeded);
    assert_eq!(store.counts().unwrap()["running"], 1);
    assert_eq!(store.events("first", 100).unwrap().len(), 6);
    assert_eq!(store.list(1).unwrap().len(), 1);
}

#[test]
fn concurrent_workers_cannot_acquire_the_same_attempt() {
    let host = Host::new();
    Store::open(&host.0, true)
        .unwrap()
        .unwrap()
        .create(example("race", 100), "operator")
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let path = host.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path, true).unwrap().unwrap();
                store
                    .set_busy_timeout(std::time::Duration::from_millis(200))
                    .unwrap();
                barrier.wait();
                store.start("race", "agent", 10, 100).is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| usize::from(w.join().unwrap()))
            .sum::<usize>(),
        1
    );
}

#[test]
fn invalid_ids_and_terminal_transitions_are_rejected_without_partial_events() {
    let host = Host::new();
    let mut store = Store::open(&host.0, true).unwrap().unwrap();
    assert!(store.create(example("../escape", 100), "operator").is_err());
    store.create(example("cancel", 100), "operator").unwrap();
    store.assign("cancel", "worker", 101).unwrap();
    store.cancel("cancel", "worker", 102).unwrap();
    assert!(store.assign("cancel", "agent", 103).is_err());
    assert!(store.start("cancel", "worker", 10, 103).is_err());
    assert!(store.retry("cancel", "worker", 103).is_err());
    assert_eq!(store.events("cancel", 100).unwrap().len(), 3);
}

fn create_command(host: &Host, id: &str, program: &str) {
    host.ok(&[
        "task",
        "create",
        "--id",
        id,
        "--title",
        "bounded execution",
        "--owner",
        "agent",
        "--session",
        "session-1",
        "--message",
        "message-1",
        "--",
        "python3",
        "-c",
        program,
    ]);
}

fn receipt(host: &Host, task: &Value) -> Value {
    let path = Path::new(task["receipt"]["path"].as_str().unwrap());
    assert!(path.starts_with(&host.0));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn real_command_has_bounded_logs_and_a_content_bound_receipt() {
    let host = Host::new();
    create_command(
        &host,
        "output",
        "import sys; sys.stdout.write('x'*8192); sys.stderr.write('error detail')",
    );
    let task = host.ok(&[
        "task",
        "run",
        "output",
        "--owner",
        "agent",
        "--log-bytes",
        "64",
    ]);
    assert_eq!(task["state"], "succeeded");
    assert_eq!(task["attempt"], 1);
    let report = receipt(&host, &task);
    assert_eq!(report["stdout"]["retained_bytes"], 64);
    assert_eq!(report["stdout"]["discarded_bytes"], 8128);
    assert_eq!(report["stderr"]["retained_bytes"], 12);
    assert_eq!(report["session_id"], "session-1");
    assert_eq!(report["message_id"], "message-1");
    assert_eq!(
        std::fs::metadata(host.0.join(".agents/forge/runs/output/1/stdout.log"))
            .unwrap()
            .len(),
        64
    );
    let status = host.ok(&["status", "--json", "--verify-receipts"]);
    assert_eq!(status["tasks"]["tasks"][0]["receipt_status"], "verified");
    std::fs::write(task["receipt"]["path"].as_str().unwrap(), "changed").unwrap();
    let output = host.cli(&["status", "--json", "--verify-receipts", "--check"]);
    assert!(!output.status.success());
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["tasks"]["tasks"][0]["receipt_status"], "changed");
}

#[test]
fn failed_commands_resume_explicitly_and_exhaust_the_budget() {
    let host = Host::new();
    create_command(&host, "failure", "raise SystemExit(7)");
    for attempt in 1..=3 {
        let mut args = vec!["task", "run", "failure", "--owner", "agent"];
        if attempt > 1 {
            args.push("--resume");
        }
        let output = host.cli(&args);
        assert!(!output.status.success());
        let task: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(task["state"], "failed");
        assert_eq!(task["exit_code"], 7);
        assert_eq!(task["attempt"], attempt);
    }
    let output = host.cli(&["task", "run", "failure", "--owner", "agent", "--resume"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("attempt budget"));
}

#[test]
fn deadline_stops_the_command_and_records_timeout() {
    let host = Host::new();
    create_command(&host, "timeout", "import time; time.sleep(20)");
    let began = Instant::now();
    let output = host.cli(&[
        "task",
        "run",
        "timeout",
        "--owner",
        "agent",
        "--timeout-seconds",
        "1",
        "--lease-seconds",
        "2",
    ]);
    assert!(began.elapsed().as_secs_f64() < 4.0);
    assert!(!output.status.success());
    let task: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(task["state"], "failed");
    assert_eq!(receipt(&host, &task)["timed_out"], true);
}

#[test]
fn status_and_listing_do_not_initialize_or_consume_state() {
    let host = Host::new();
    assert_eq!(host.ok(&["task", "list"])["total"], 0);
    assert_eq!(
        host.ok(&["status", "--json"])["tasks"]["status"],
        "not_initialized"
    );
    assert!(!host.0.join(".agents").exists());
    let agent = host.0.join(".agents/a2a/agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.parent().unwrap().join("agents.json"),
        json!({"schema_version":1,"agents":{"agent":{"port":8000,"token":"private-token"}}})
            .to_string(),
    )
    .unwrap();
    let database = rusqlite::Connection::open(agent.join("store.sqlite")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE messages(direction TEXT,delivery_status TEXT,read_at TEXT);
        INSERT INTO messages VALUES ('outbound','queued',NULL),('inbound','received',NULL);",
        )
        .unwrap();
    let output = host.cli(&["status", "--json", "--check"]);
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private-token"));
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        status["messaging"]["agents"][0]["mailbox"]["inbound_unread"],
        1
    );
    let unread: i64 = database
        .query_row(
            "SELECT count(*) FROM messages WHERE read_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unread, 2);
}

#[test]
fn malformed_state_is_an_error_never_a_clean_zero() {
    let host = Host::new();
    std::fs::create_dir_all(host.0.join(".agents/a2a")).unwrap();
    std::fs::write(host.0.join(".agents/a2a/agents.json"), "not json").unwrap();
    let output = host.cli(&["status", "--json"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["messaging"]["status"], "error");
    assert!(!report["errors"].as_array().unwrap().is_empty());
}

#[test]
fn status_joins_only_host_session_totals_without_double_counting_tasks() {
    let host = Host::new();
    create_command(&host, "one", "pass");
    create_command(&host, "two", "pass");
    let directory = host.0.join("ledger/session_rollup");
    std::fs::create_dir_all(&directory).unwrap();
    let row =
        json!({"session_id":"session-1","project":host.0,"total_input":500,"total_output":20});
    std::fs::write(directory.join("2026-09-26.jsonl"), format!("{row}\n")).unwrap();
    let status = host.ok(&["status", "--json"]);
    assert_eq!(status["ledger"]["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(status["ledger"]["sessions"][0]["total_input"], 500);
    assert_eq!(status["tasks"]["total"], 2);
    let foreign = json!({"session_id":"session-1","project":"/different-host","total_input":999});
    std::fs::write(directory.join("2026-09-27.jsonl"), format!("{foreign}\n")).unwrap();
    assert_eq!(
        host.ok(&["status", "--json"])["ledger"]["sessions"][0]["total_input"],
        500
    );
}

#[cfg(unix)]
fn wait_for_child_pid(host: &Host) -> i32 {
    let began = Instant::now();
    let path = host.0.join("child.pid");
    loop {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(pid) = raw.parse() {
                return pid;
            }
        }
        assert!(began.elapsed().as_secs() < 3, "child did not start");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn supervisor_sigterm_kills_its_child_and_records_failure() {
    let host = Host::new();
    create_command(&host, "signal", "import os,time,pathlib; pathlib.Path('child.pid').write_text(str(os.getpid())); time.sleep(20)");
    let mut runner = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["task", "run", "signal", "--owner", "agent"])
        .current_dir(&host.0)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child = wait_for_child_pid(&host);
    let began = Instant::now();
    assert_eq!(unsafe { libc::kill(runner.id() as i32, libc::SIGTERM) }, 0);
    while runner.try_wait().unwrap().is_none() {
        assert!(
            began.elapsed().as_secs() < 3,
            "supervisor did not stop after SIGTERM"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
    let task = host.ok(&["task", "show", "signal"]);
    assert_eq!(task["task"]["state"], "failed");
    assert!(task["task"]["detail"].as_str().unwrap().contains("signal"));
}

#[cfg(unix)]
#[test]
fn sqlite_contention_cannot_extend_command_execution_or_report_success() {
    let host = Host::new();
    create_command(&host, "locked", "import os,time,pathlib; pathlib.Path('child.pid').write_text(str(os.getpid())); time.sleep(4)");
    let mut runner = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args([
            "task",
            "run",
            "locked",
            "--owner",
            "agent",
            "--timeout-seconds",
            "1",
            "--lease-seconds",
            "2",
        ])
        .current_dir(&host.0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child = wait_for_child_pid(&host);
    let lock = rusqlite::Connection::open(task_store::path(&host.0)).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let began = Instant::now();
    while runner.try_wait().unwrap().is_none() {
        assert!(
            began.elapsed().as_secs_f64() < 2.5,
            "SQLite contention defeated deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
    lock.execute_batch("ROLLBACK").unwrap();
    assert_ne!(
        task_store::required(&host.0)
            .unwrap()
            .get("locked")
            .unwrap()
            .state,
        State::Succeeded
    );
}
