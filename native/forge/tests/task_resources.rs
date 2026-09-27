//! Native resource admission and independently verified process-tree limits.
#![cfg(target_os = "linux")]

mod support;

use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use support::{task_worker, Host};

fn create(host: &Host, id: &str, worker_args: &[&str]) {
    let mut args = vec![
        "task",
        "create",
        "--id",
        id,
        "--title",
        "resource test",
        "--owner",
        "agent",
        "--",
        task_worker().to_str().unwrap(),
    ];
    args.extend_from_slice(worker_args);
    host.ok(&args);
}

fn receipt(host: &Host, id: &str) -> Value {
    let task = host.ok(&["task", "show", id]);
    let path = task["task"]["receipt"]["path"].as_str().unwrap();
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn resource_preview_does_not_initialize_state() {
    let host = Host::new();
    let report = host.ok(&["task", "resources"]);
    assert_eq!(report["admitted"], true);
    assert!(report["available"]["allowed_cpus"].as_u64().unwrap() > 0);
    assert!(
        report["available"]["memory_available_mib"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["available"]["gpu"].is_null());
    assert!(!host.0.join(".agents").exists());
}

#[test]
fn default_execution_masks_all_gpu_runtimes_and_records_admission() {
    let host = Host::new();
    create(
        &host,
        "cpu",
        &[
            "env",
            "CUDA_VISIBLE_DEVICES",
            "NVIDIA_VISIBLE_DEVICES",
            "ROCR_VISIBLE_DEVICES",
            "HIP_VISIBLE_DEVICES",
        ],
    );
    let output = host
        .command()
        .env("CUDA_VISIBLE_DEVICES", "0")
        .env("NVIDIA_VISIBLE_DEVICES", "all")
        .env("ROCR_VISIBLE_DEVICES", "0")
        .env("HIP_VISIBLE_DEVICES", "0")
        .args(["task", "run", "cpu", "--owner", "agent"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = receipt(&host, "cpu");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["succeeded"], true);
    assert!(report["resources"]["available"]["gpu"].is_null());
    assert!(report["resources"]["enforcement"].is_null());
    let log = std::fs::read_to_string(host.0.join(".agents/forge/runs/cpu/1/stdout.log")).unwrap();
    assert_eq!(log, "CUDA_VISIBLE_DEVICES=\nNVIDIA_VISIBLE_DEVICES=void\nROCR_VISIBLE_DEVICES=\nHIP_VISIBLE_DEVICES=\n");
}

#[test]
fn rejected_admission_preserves_failed_state_and_retry_budget() {
    let host = Host::new();
    create(&host, "retry", &["exit", "7"]);
    assert!(!host
        .cli(&["task", "run", "retry", "--owner", "agent"])
        .status
        .success());
    let before = host.ok(&["task", "show", "retry"]);
    let result = host.cli(&[
        "task",
        "run",
        "retry",
        "--owner",
        "agent",
        "--resume",
        "--memory-mib",
        "18446744073709551615",
    ]);
    assert!(!result.status.success());
    let after = host.ok(&["task", "show", "retry"]);
    assert_eq!(before, after);
    assert_eq!(after["task"]["attempt"], 1);
    assert_eq!(after["task"]["state"], "failed");
}

#[test]
fn missing_limit_backend_refuses_before_start() {
    let host = Host::new();
    create(&host, "backend", &["exit", "0"]);
    let before = host.ok(&["task", "show", "backend"]);
    let output = host
        .command()
        .env("PATH", host.0.join("no-tools"))
        .args([
            "task", "run", "backend", "--owner", "agent", "--cpus", "0.1",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(host.ok(&["task", "show", "backend"]), before);
}

fn scoped_run(host: &Host, id: &str, timeout: &str) -> std::process::Output {
    host.cli(&[
        "task",
        "run",
        id,
        "--owner",
        "agent",
        "--cpus",
        "0.5",
        "--memory-mib",
        "64",
        "--timeout-seconds",
        timeout,
    ])
}

#[test]
#[ignore = "requires a live user systemd manager and delegated CPU/memory controllers"]
fn real_scope_enforces_limits_and_preserves_literal_arguments() {
    let host = Host::new();
    create(
        &host,
        "scope",
        &["echo", "$HOME", "${PATH}", "hello world", "--flag"],
    );
    let result = scoped_run(&host, "scope", "10");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report = receipt(&host, "scope");
    assert_eq!(report["succeeded"], true);
    assert_eq!(report["resources"]["requested"]["cpus"], 0.5);
    assert_eq!(report["resources"]["enforcement"]["verified"], true);
    let log =
        std::fs::read_to_string(host.0.join(".agents/forge/runs/scope/1/stdout.log")).unwrap();
    assert_eq!(log, "$HOME\n${PATH}\nhello world\n--flag\n");
}

fn assert_gone(path: &Path) {
    let pid: u32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with('Z') => return,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!("detached worker {pid} still alive");
}

#[test]
#[ignore = "requires a live user systemd manager and delegated CPU/memory controllers"]
fn real_scope_cleans_detached_descendant_after_leader_success() {
    let host = Host::new();
    let pid = host.0.join("detached.pid");
    create(
        &host,
        "detached",
        &["detach-exit", "20000", pid.to_str().unwrap()],
    );
    let result = scoped_run(&host, "detached", "10");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_gone(&pid);
}

#[test]
#[ignore = "requires a live user systemd manager and delegated CPU/memory controllers"]
fn real_memory_limit_fails_only_the_bounded_attempt() {
    let host = Host::new();
    create(&host, "memory", &["allocate", "128"]);
    let result = scoped_run(&host, "memory", "10");
    assert!(!result.status.success());
    let report = receipt(&host, "memory");
    assert_eq!(report["succeeded"], false);
    assert_eq!(report["resources"]["enforcement"]["verified"], true);
    assert_eq!(
        host.ok(&["task", "show", "memory"])["task"]["state"],
        "failed"
    );
}
