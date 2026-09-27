//! Cancellation before admission completes must leave the task unstarted.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
use support::{task_worker, Host};

fn create(host: &Host, id: &str, workload: &[&str]) {
    let mut args = vec![
        "task",
        "create",
        "--id",
        id,
        "--title",
        "preflight and cleanup regression",
        "--owner",
        "agent",
        "--",
        task_worker().to_str().unwrap(),
    ];
    args.extend_from_slice(workload);
    host.ok(&args);
}

fn wait_for_probe(runner: &mut Child, path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match std::fs::read_to_string(path) {
            Ok(raw) if raw.parse::<u32>().is_ok() => return,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading probe readiness: {error}"),
        }
        assert!(
            runner.try_wait().unwrap().is_none(),
            "task runner exited before the capability probe started"
        );
        if Instant::now() >= deadline {
            runner.kill().unwrap();
            runner.wait().unwrap();
            panic!("capability probe did not start");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn cancellation_during_backend_probe_preserves_the_unstarted_task() {
    let host = Host::new();
    let payload_pid = host.0.join("payload.pid");
    create(
        &host,
        "preflight",
        &["pid-sleep", "1000", payload_pid.to_str().unwrap()],
    );
    let before = host.ok(&["task", "show", "preflight"]);
    let tools = host.0.join("probe-tools");
    std::fs::create_dir(&tools).unwrap();
    for name in ["systemctl", "systemd-run"] {
        std::os::unix::fs::symlink(task_worker(), tools.join(name)).unwrap();
    }
    let probe_pid = host.0.join("probe.pid");
    let mut runner = host
        .command()
        .args([
            "task",
            "run",
            "preflight",
            "--owner",
            "agent",
            "--cpus",
            "0.5",
            "--memory-mib",
            "64",
            "--timeout-seconds",
            "2",
            "--lease-seconds",
            "3",
        ])
        .env("PATH", &tools)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", host.0.join("probe-runtime/bus").display()),
        )
        .env("XDG_RUNTIME_DIR", host.0.join("probe-runtime"))
        .env("FORGE_TEST_PREFLIGHT_PROBE_PID", &probe_pid)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_probe(&mut runner, &probe_pid);
    assert_eq!(unsafe { libc::kill(runner.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(4);
    while runner.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            runner.kill().unwrap();
            runner.wait().unwrap();
            panic!("cancelled task runner did not stop");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = runner.wait_with_output().unwrap();
    assert!(!output.status.success());
    let after = host.ok(&["task", "show", "preflight"]);
    assert_eq!(after["task"]["state"], "assigned");
    assert_eq!(after["task"]["attempt"], 0);
    assert_eq!(after, before);
    assert!(
        !payload_pid.exists(),
        "cancelled preflight launched payload"
    );
}

#[test]
#[ignore = "requires a live user systemd manager and delegated CPU/memory controllers"]
fn cleanup_longer_than_the_execution_lease_still_binds_a_success_receipt() {
    let host = Host::new();
    create(&host, "cleanup", &["echo", "literal $HOME"]);
    let tools = host.0.join("slow-tools");
    std::fs::create_dir(&tools).unwrap();
    std::os::unix::fs::symlink(task_worker(), tools.join("systemctl")).unwrap();
    let mut paths = vec![tools];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let began = Instant::now();
    let output = host
        .command()
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("FORGE_TEST_SLOW_CLEANUP", "1")
        .args([
            "task",
            "run",
            "cleanup",
            "--owner",
            "agent",
            "--cpus",
            "0.5",
            "--memory-mib",
            "64",
            "--lease-seconds",
            "2",
            "--timeout-seconds",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(began.elapsed() >= Duration::from_millis(2200));
    let task = host.ok(&["task", "show", "cleanup"]);
    assert_eq!(task["task"]["state"], "succeeded");
    let receipt_path = task["task"]["receipt"]["path"].as_str().unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(report["succeeded"], true);
    assert_eq!(report["resources"]["enforcement"]["verified"], true);
    let status = host.ok(&["status", "--json", "--verify-receipts"]);
    assert_eq!(status["tasks"]["tasks"][0]["receipt_status"], "verified");
    assert_eq!(
        std::fs::read_to_string(host.0.join(".agents/forge/runs/cleanup/1/stdout.log")).unwrap(),
        "literal $HOME\n"
    );
}

struct ProbeChildCleanup(PathBuf, bool);

impl Drop for ProbeChildCleanup {
    fn drop(&mut self) {
        if !self.1 {
            return;
        }
        let raw = match std::fs::read_to_string(&self.0) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                eprintln!("reading owned probe descendant PID for cleanup: {error}");
                return;
            }
        };
        let pid = match raw.parse::<i32>() {
            Ok(pid) if pid > 1 => pid,
            _ => {
                eprintln!("invalid owned probe descendant PID: {raw:?}");
                return;
            }
        };
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                eprintln!("cleaning owned probe descendant {pid}: {error}");
            }
        }
    }
}

#[test]
fn successful_gpu_probe_removes_its_same_group_descendant() {
    const GPU: &str = "GPU-aaaaaaaa-1111-2222-3333-444444444444";
    let host = Host::new();
    let tools = host.0.join("gpu-probe-tools");
    std::fs::create_dir(&tools).unwrap();
    std::os::unix::fs::symlink(task_worker(), tools.join("nvidia-smi")).unwrap();
    let mut cleanup = ProbeChildCleanup(host.0.join("gpu-probe-child.pid"), true);
    let output = host
        .command()
        .args(["task", "resources", "--gpu", GPU])
        .env("PATH", &tools)
        .env("CUDA_VISIBLE_DEVICES", GPU)
        .env("NVIDIA_VISIBLE_DEVICES", GPU)
        .env("FORGE_TEST_GPU_PROBE_PID", &cleanup.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["admitted"], true);
    assert_eq!(report["available"]["gpu"]["uuid"], GPU);
    let pid: u32 = std::fs::read_to_string(&cleanup.0)
        .unwrap()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => panic!("reading probe descendant state: {error}"),
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with('Z') => break,
            Ok(_) => {}
        }
        assert!(
            Instant::now() < deadline,
            "successful probe left descendant {pid} alive"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    cleanup.1 = false;
}
