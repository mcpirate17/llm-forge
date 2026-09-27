//! Owned subprocess and PyO3 fixtures for process-lifetime contracts.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub fn child_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mutation_support_child")
}

pub fn subject(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.mutation_testing_support")
}

pub fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || *libc::__errno_location() == libc::EPERM }
}

pub fn wait_dead(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while pid_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

pub fn marker_pid(path: &Path) -> i32 {
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

pub fn wait_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(path.exists(), "fixture did not publish {}", path.display());
}

pub fn run_command<'py>(
    py: Python<'py>,
    argv: &[String],
    cwd: &Path,
    registry: &Path,
    timeout: i32,
    tail: i32,
) -> PyResult<Bound<'py, PyAny>> {
    let builtins = module(py, "builtins");
    let kw = PyDict::new(py);
    kw.set_item("cwd", path(py, cwd)).unwrap();
    kw.set_item("timeout_seconds", timeout).unwrap();
    kw.set_item("environment", PyDict::new(py)).unwrap();
    kw.set_item("pin_argv", builtins.getattr("list").unwrap())
        .unwrap();
    let dataclasses = module(py, "dataclasses");
    let fields = [
        "returncode",
        "timed_out",
        "duration_seconds",
        "stdout_tail",
        "stderr_tail",
    ];
    let options = PyDict::new(py);
    options.set_item("frozen", true).unwrap();
    let result_class = dataclasses
        .getattr("make_dataclass")
        .unwrap()
        .call(("_Result", fields), Some(&options))
        .unwrap();
    kw.set_item("result_factory", result_class).unwrap();
    kw.set_item("output_tail_chars", tail).unwrap();
    kw.set_item("pgid_registry", path(py, registry)).unwrap();
    subject(py)
        .getattr("run_command")?
        .call((PyList::new(py, argv)?,), Some(&kw))
}

pub struct OwnedProcesses {
    children: Vec<Child>,
    groups: Vec<i32>,
    markers: Vec<PathBuf>,
}

impl OwnedProcesses {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            groups: Vec::new(),
            markers: Vec::new(),
        }
    }

    pub fn spawn_group(&mut self, command: &mut Command) -> i32 {
        command.stdout(Stdio::null()).stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let pid = child.id() as i32;
        self.groups.push(pid);
        self.children.push(child);
        pid
    }

    pub fn spawn_child(&mut self, command: &mut Command) -> i32 {
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let child = command.spawn().unwrap();
        let pid = child.id() as i32;
        self.children.push(child);
        pid
    }

    pub fn track_group(&mut self, pgid: i32) {
        self.groups.push(pgid);
    }

    pub fn track_marker(&mut self, marker: &Path) {
        self.markers.push(marker.to_path_buf());
    }

    pub fn wait_child(&mut self, pid: i32) -> std::process::ExitStatus {
        let child = self
            .children
            .iter_mut()
            .find(|child| child.id() as i32 == pid)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "fixture child {pid} exceeded 10s wait"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for OwnedProcesses {
    fn drop(&mut self) {
        let our_group = unsafe { libc::getpgrp() };
        for marker in &self.markers {
            if let Ok(pid) = fs::read_to_string(marker)
                .unwrap_or_default()
                .trim()
                .parse::<i32>()
            {
                let group = unsafe { libc::getpgid(pid) };
                if group > 0 && group != our_group {
                    unsafe { libc::killpg(group, libc::SIGKILL) };
                }
            }
        }
        for group in &self.groups {
            if *group > 0 && *group != our_group {
                unsafe { libc::killpg(*group, libc::SIGKILL) };
            }
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
