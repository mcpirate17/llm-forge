//! Linux process-tree limits. The scoped Rust guard verifies kernel settings
//! before executing the user's argv; requested settings alone are not evidence.

use crate::task_resources::{run_probe, ResourceRequest, ResourceSnapshot};
use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
static NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LimitsReport {
    pub backend: String,
    pub unit: String,
    pub verified: bool,
    pub cgroup: Option<String>,
    pub cpu_quota_us: Option<u64>,
    pub cpu_period_us: Option<u64>,
    pub memory_max_bytes: Option<u64>,
    pub memory_swap_max_bytes: Option<u64>,
    /// RuntimeMaxSec is submitted to systemd; CPU/memory verification is read
    /// from the kernel. This field deliberately does not claim runtime readback.
    pub requested_runtime_seconds: u64,
}

pub struct LimitScope {
    unit: String,
    cpus: Option<f64>,
    memory_bytes: Option<u64>,
    timeout_seconds: u64,
    environment: Vec<(OsString, OsString)>,
    executable: PathBuf,
    proof: Option<PathBuf>,
}

/// Internal launch guard. Success replaces this process with the literal argv.
#[derive(Args)]
pub struct LimitExecArgs {
    #[arg(long)]
    unit: String,
    #[arg(long)]
    cpus: Option<f64>,
    #[arg(long)]
    memory_bytes: Option<u64>,
    #[arg(long)]
    timeout_seconds: u64,
    #[arg(long)]
    proof: PathBuf,
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

fn unit_name(host: &Path, task: &str, nonce: u128, sequence: u64, pid: u32) -> String {
    let identity = format!("{}\0{task}\0{nonce}\0{sequence}\0{pid}", host.display());
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    format!("forge-task-{}-{pid}.scope", &digest[..32])
}

#[cfg(unix)]
fn verified_runtime(runtime: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = std::fs::symlink_metadata(runtime).context("user runtime directory")?;
    ensure!(
        directory.is_dir() && directory.uid() == uid,
        "user runtime directory is not owned by this UID"
    );
    let socket =
        std::fs::symlink_metadata(runtime.join("bus")).context("user manager bus socket")?;
    ensure!(
        socket.file_type().is_socket() && socket.uid() == uid,
        "user bus is not a socket owned by this UID"
    );
    Ok(())
}

fn user_environment() -> Result<Vec<(OsString, OsString)>> {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        let configured = std::env::var_os("XDG_RUNTIME_DIR");
        let runtime = configured
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let mut environment = Vec::new();
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            verified_runtime(&runtime, uid)?;
            let mut address = OsString::from("unix:path=");
            address.push(runtime.join("bus"));
            environment.push(("DBUS_SESSION_BUS_ADDRESS".into(), address));
            if configured.is_none() {
                environment.push(("XDG_RUNTIME_DIR".into(), runtime.into_os_string()));
            }
        } else if configured.is_none() && verified_runtime(&runtime, uid).is_ok() {
            environment.push(("XDG_RUNTIME_DIR".into(), runtime.into_os_string()));
        }
        Ok(environment)
    }
    #[cfg(not(unix))]
    bail!("systemd process-tree limits require Linux")
}

fn successful(command: &mut Command) -> Result<std::process::Output> {
    let output = run_probe(command, PROBE_TIMEOUT)?;
    ensure!(
        output.status.success(),
        "resource backend command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

/// A new user scope is a sibling of the caller. Carry its observed caps across
/// that boundary; this does not preserve an aggregate budget shared by siblings.
fn scope_limits(
    request: &ResourceRequest,
    available: &ResourceSnapshot,
) -> Result<(Option<f64>, Option<u64>)> {
    ensure!(
        available.allowed_cpus > 0
            && available.effective_cpus.is_finite()
            && available.effective_cpus > 0.0,
        "invalid observed CPU capacity"
    );
    let cpus = request.cpus.or_else(|| {
        (available.effective_cpus < available.allowed_cpus as f64)
            .then_some(available.effective_cpus)
    });
    if let Some(cpus) = cpus {
        ensure!(
            cpus.is_finite() && cpus >= 0.01 && cpus <= available.effective_cpus,
            "CPU scope quota must be at least 0.01 CPUs and not exceed observed capacity"
        );
    }
    if let Some(memory) = request.memory_mib {
        ensure!(
            memory <= available.memory_available_mib
                && available
                    .cgroup_memory_limit_mib
                    .is_none_or(|limit| memory <= limit),
            "memory request exceeds observed available RAM or ancestor limit"
        );
    }
    let memory_bytes = request
        .memory_mib
        .or(available.cgroup_memory_limit_mib)
        .map(|mib| {
            ensure!(mib > 0, "memory limit must be positive");
            mib.checked_mul(1024 * 1024)
                .context("memory limit overflow")
        })
        .transpose()?;
    Ok((cpus, memory_bytes))
}

impl LimitScope {
    pub fn prepare(
        host: &Path,
        task_id: &str,
        request: &ResourceRequest,
        available: &ResourceSnapshot,
        timeout_seconds: u64,
    ) -> Result<Option<Self>> {
        if request.cpus.is_none() && request.memory_mib.is_none() {
            return Ok(None);
        }
        ensure!(
            cfg!(target_os = "linux"),
            "hard process-tree CPU/memory limits require Linux and a usable systemd user manager"
        );
        ensure!(
            (1..=86400).contains(&timeout_seconds),
            "invalid scope runtime limit"
        );
        let (cpus, memory_bytes) = scope_limits(request, available)?;
        let environment = user_environment()?;
        let mut manager = Command::new("systemctl");
        manager
            .args([
                "--user",
                "--no-pager",
                "show",
                "--property=Version",
                "--value",
            ])
            .envs(environment.iter().cloned());
        successful(&mut manager)
            .context("systemd user manager is unavailable; no task was launched")?;
        successful(Command::new("systemd-run").arg("--version"))
            .context("systemd-run is unavailable; no task was launched")?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Some(Self {
            unit: unit_name(
                &host.canonicalize()?,
                task_id,
                nonce,
                NONCE.fetch_add(1, Ordering::Relaxed),
                std::process::id(),
            ),
            cpus,
            memory_bytes,
            timeout_seconds,
            environment,
            executable: std::env::current_exe().context("finding native launch guard")?,
            proof: None,
        }))
    }

    pub fn command(&mut self, program: &str, args: &[String], directory: &Path) -> Command {
        let proof = directory.join("resources.json");
        self.proof = Some(proof.clone());
        let mut command = Command::new("systemd-run");
        command
            .args([
                "--user",
                "--scope",
                "--quiet",
                "--no-ask-password",
                "--expand-environment=no",
            ])
            .arg(format!("--unit={}", self.unit))
            .arg(format!(
                "--property=RuntimeMaxSec={}s",
                self.timeout_seconds
            ))
            .args([
                "--property=TimeoutStopSec=1s",
                "--property=KillMode=control-group",
            ])
            .envs(self.environment.iter().cloned());
        if let Some(cpus) = self.cpus {
            command.arg(format!("--property=CPUQuota={}%", cpus * 100.0));
            command.arg("--property=CPUQuotaPeriodSec=100ms");
        }
        if let Some(bytes) = self.memory_bytes {
            command.arg(format!("--property=MemoryMax={bytes}"));
            command.arg("--property=MemorySwapMax=0");
        }
        command
            .arg("--")
            .arg(&self.executable)
            .arg("task-limit-exec")
            .arg("--unit")
            .arg(&self.unit)
            .arg("--timeout-seconds")
            .arg(self.timeout_seconds.to_string())
            .arg("--proof")
            .arg(proof);
        if let Some(cpus) = self.cpus {
            command.arg("--cpus").arg(cpus.to_string());
        }
        if let Some(bytes) = self.memory_bytes {
            command.arg("--memory-bytes").arg(bytes.to_string());
        }
        command.arg("--").arg(program).args(args);
        command
    }

    fn control(&self, args: &[&str]) -> Command {
        let mut command = Command::new("systemctl");
        command
            .args(["--user", "--no-pager", "--no-ask-password"])
            .args(args)
            .arg(&self.unit)
            .envs(self.environment.iter().cloned());
        command
    }

    fn absent(&self) -> Result<bool> {
        let output = run_probe(
            &mut self.control(&["show", "--property=LoadState", "--property=ActiveState"]),
            PROBE_TIMEOUT,
        )?;
        let state = String::from_utf8_lossy(&output.stdout);
        if state
            .lines()
            .any(|line| line == "LoadState=not-found" || line == "ActiveState=inactive")
        {
            return Ok(true);
        }
        ensure!(
            output.status.success(),
            "cannot inspect owned scope {}: {}",
            self.unit,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(false)
    }

    pub fn stop(&self) -> Result<()> {
        if self.absent()? {
            return Ok(());
        }
        let output = run_probe(&mut self.control(&["stop"]), PROBE_TIMEOUT)?;
        if !output.status.success() && !self.absent()? {
            bail!(
                "could not stop owned resource scope {}: {}",
                self.unit,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        ensure!(
            self.absent()?,
            "owned resource scope {} remained active after stop",
            self.unit
        );
        Ok(())
    }

    pub fn unverified_report(&self) -> LimitsReport {
        LimitsReport {
            backend: "systemd_user_scope".into(),
            unit: self.unit.clone(),
            verified: false,
            cgroup: None,
            cpu_quota_us: None,
            cpu_period_us: None,
            memory_max_bytes: None,
            memory_swap_max_bytes: None,
            requested_runtime_seconds: self.timeout_seconds,
        }
    }

    pub fn report(&self) -> Result<LimitsReport> {
        let path = self
            .proof
            .as_ref()
            .context("resource scope was not launched")?;
        ensure!(
            std::fs::metadata(path)?.len() <= 16 * 1024,
            "resource proof exceeds its size bound"
        );
        let report: LimitsReport = serde_json::from_reader(File::open(path)?)
            .context("native launch guard did not leave complete resource proof")?;
        ensure!(
            report.verified
                && report.unit == self.unit
                && report.backend == "systemd_user_scope"
                && report.requested_runtime_seconds == self.timeout_seconds,
            "resource proof identity mismatch"
        );
        validate_report(&report, self.cpus, self.memory_bytes)?;
        Ok(report)
    }
}

fn controller_limit(text: &str) -> Result<Option<u64>> {
    if text.trim() == "max" {
        return Ok(None);
    }
    Ok(Some(text.trim().parse().context("invalid cgroup limit")?))
}

fn cpu_limit(text: &str) -> Result<(Option<u64>, u64)> {
    let words: Vec<_> = text.split_whitespace().collect();
    ensure!(words.len() == 2, "invalid cpu.max record");
    let period = words[1].parse().context("invalid CPU quota period")?;
    ensure!(period > 0, "zero CPU quota period");
    Ok((controller_limit(words[0])?, period))
}

fn validate_report(report: &LimitsReport, cpus: Option<f64>, memory: Option<u64>) -> Result<()> {
    let cgroup = report
        .cgroup
        .as_deref()
        .context("resource proof has no cgroup")?;
    ensure!(
        Path::new(cgroup)
            .file_name()
            .is_some_and(|name| name == report.unit.as_str()),
        "resource proof refers to another scope"
    );
    if let Some(cpus) = cpus {
        let quota = report.cpu_quota_us.context("CPU controller is unlimited")?;
        let period = report
            .cpu_period_us
            .context("CPU controller period is unavailable")?;
        ensure!(
            period > 0 && quota > 0 && quota as f64 / period as f64 <= cpus + 1e-9,
            "CPU quota exceeds the requested budget"
        );
    }
    if let Some(memory) = memory {
        let actual = report
            .memory_max_bytes
            .context("memory controller is unlimited")?;
        ensure!(
            actual <= memory && report.memory_swap_max_bytes == Some(0),
            "memory or swap setting exceeds the requested budget"
        );
    }
    Ok(())
}

fn scoped_cgroup(text: &str, unit: &str) -> Result<PathBuf> {
    let relative = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("a unified cgroup v2 entry is required")?;
    let path = Path::new(relative);
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "invalid cgroup path"
    );
    ensure!(
        path.file_name().is_some_and(|name| name == unit),
        "launch guard is not inside its owned scope"
    );
    Ok(Path::new("/sys/fs/cgroup").join(path.strip_prefix("/")?))
}

fn guard_report(args: &LimitExecArgs) -> Result<LimitsReport> {
    ensure!(
        args.unit.starts_with("forge-task-")
            && args.unit.ends_with(".scope")
            && args
                .unit
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.')),
        "invalid owned scope name"
    );
    ensure!(
        args.cpus.is_some() || args.memory_bytes.is_some(),
        "guard requires a resource limit"
    );
    if let Some(cpus) = args.cpus {
        ensure!(cpus.is_finite() && cpus > 0.0, "invalid CPU request");
    }
    let cgroup = scoped_cgroup(&std::fs::read_to_string("/proc/self/cgroup")?, &args.unit)?;
    let (quota, period) = if args.cpus.is_some() {
        let (quota, period) = cpu_limit(&std::fs::read_to_string(cgroup.join("cpu.max"))?)?;
        (quota, Some(period))
    } else {
        (None, None)
    };
    let (memory, swap) = if args.memory_bytes.is_some() {
        (
            controller_limit(&std::fs::read_to_string(cgroup.join("memory.max"))?)?,
            controller_limit(&std::fs::read_to_string(cgroup.join("memory.swap.max"))?)?,
        )
    } else {
        (None, None)
    };
    let report = LimitsReport {
        backend: "systemd_user_scope".into(),
        unit: args.unit.clone(),
        verified: true,
        cgroup: Some(cgroup.display().to_string()),
        cpu_quota_us: quota,
        cpu_period_us: period,
        memory_max_bytes: memory,
        memory_swap_max_bytes: swap,
        requested_runtime_seconds: args.timeout_seconds,
    };
    validate_report(&report, args.cpus, args.memory_bytes)?;
    Ok(report)
}

pub fn run_guard(args: LimitExecArgs) -> Result<u8> {
    ensure!(
        cfg!(target_os = "linux"),
        "resource launch guard requires Linux"
    );
    let (program, arguments) = args
        .command
        .split_first()
        .context("guard requires literal command argv")?;
    let report = guard_report(&args)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&args.proof)
        .context("creating resource verification proof")?;
    serde_json::to_writer(&mut file, &report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(Command::new(program).args(arguments).exec())
            .context("executing the resource-verified command")
    }
    #[cfg(not(unix))]
    bail!("resource launch guard requires Unix exec")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> ResourceSnapshot {
        ResourceSnapshot {
            captured_at: 0,
            allowed_cpus: 8,
            effective_cpus: 2.0,
            load_1m: 0.0,
            load_5m: 0.0,
            load_15m: 0.0,
            memory_total_mib: 8192,
            memory_available_mib: 1024,
            cgroup_v2_path: Some("/caller.scope".into()),
            cgroup_memory_limit_mib: Some(2048),
            cgroup_memory_available_mib: Some(1024),
            gpu: None,
        }
    }

    #[test]
    fn missing_dimensions_inherit_caps_and_explicit_stricter_limits_win() {
        let snapshot = available();
        let mut request = ResourceRequest {
            cpus: Some(1.0),
            ..Default::default()
        };
        assert_eq!(
            scope_limits(&request, &snapshot).unwrap(),
            (Some(1.0), Some(2048 * 1024 * 1024))
        );
        request.cpus = None;
        request.memory_mib = Some(512);
        assert_eq!(
            scope_limits(&request, &snapshot).unwrap(),
            (Some(2.0), Some(512 * 1024 * 1024))
        );
        request.cpus = Some(0.5);
        assert_eq!(
            scope_limits(&request, &snapshot).unwrap(),
            (Some(0.5), Some(512 * 1024 * 1024))
        );
        let mut unconstrained = snapshot;
        unconstrained.effective_cpus = unconstrained.allowed_cpus as f64;
        unconstrained.cgroup_memory_limit_mib = None;
        request.cpus = None;
        assert_eq!(
            scope_limits(&request, &unconstrained).unwrap(),
            (None, Some(512 * 1024 * 1024))
        );
    }

    #[test]
    fn derived_limits_reject_invalid_or_overcommitted_requests() {
        let mut snapshot = available();
        for request in [
            ResourceRequest {
                cpus: Some(3.0),
                ..Default::default()
            },
            ResourceRequest {
                memory_mib: Some(1025),
                ..Default::default()
            },
            ResourceRequest {
                memory_mib: Some(0),
                ..Default::default()
            },
        ] {
            assert!(scope_limits(&request, &snapshot).is_err());
        }
        let request = ResourceRequest {
            memory_mib: Some(512),
            ..Default::default()
        };
        snapshot.effective_cpus = 0.001;
        assert!(scope_limits(&request, &snapshot).is_err());
        snapshot.effective_cpus = f64::NAN;
        assert!(scope_limits(&request, &snapshot).is_err());
        snapshot = available();
        snapshot.cgroup_memory_limit_mib = Some(u64::MAX);
        let cpu_only = ResourceRequest {
            cpus: Some(1.0),
            ..Default::default()
        };
        assert!(scope_limits(&cpu_only, &snapshot).is_err());
    }

    #[test]
    fn no_explicit_budget_keeps_inherited_scope_without_backend_probes() {
        assert!(LimitScope::prepare(
            Path::new("/does/not/exist"),
            "unused",
            &ResourceRequest::default(),
            &available(),
            60
        )
        .unwrap()
        .is_none());
    }

    fn scope() -> LimitScope {
        LimitScope {
            unit: "forge-task-test-123.scope".into(),
            cpus: Some(2.0),
            memory_bytes: Some(536_870_912),
            timeout_seconds: 60,
            environment: vec![],
            executable: "/bin/forge".into(),
            proof: None,
        }
    }

    #[test]
    fn names_bind_host_task_nonce_and_process_without_untrusted_fragments() {
        let base = unit_name(Path::new("/host"), "$(bad)/task", 1, 0, 2);
        assert!(base.starts_with("forge-task-") && base.ends_with("-2.scope"));
        assert!(!base.contains("bad"));
        for alternate in [
            unit_name(Path::new("/other"), "$(bad)/task", 1, 0, 2),
            unit_name(Path::new("/host"), "task", 1, 0, 2),
            unit_name(Path::new("/host"), "$(bad)/task", 2, 0, 2),
            unit_name(Path::new("/host"), "$(bad)/task", 1, 1, 2),
        ] {
            assert_ne!(base, alternate);
        }
    }

    #[test]
    fn command_preserves_literal_argv_and_configures_its_own_scope() {
        let mut scope = scope();
        let command = scope.command(
            "/bin/echo",
            &["$HOME $(false); spaces".into()],
            Path::new("/run/attempt"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--expand-environment=no".into()));
        assert!(args.contains(&"--property=CPUQuota=200%".into()));
        assert!(args.contains(&"--property=MemoryMax=536870912".into()));
        assert!(args.contains(&"--property=MemorySwapMax=0".into()));
        assert!(args.contains(&"--property=RuntimeMaxSec=60s".into()));
        assert_eq!(
            &args[args.len() - 3..],
            &["--", "/bin/echo", "$HOME $(false); spaces"]
        );
        assert_eq!(
            scope.proof,
            Some(PathBuf::from("/run/attempt/resources.json"))
        );
    }

    #[test]
    fn proof_rejects_unlimited_or_wrong_scope_and_accepts_stricter_limits() {
        let scope = scope();
        let mut report = scope.unverified_report();
        report.cgroup = Some(format!("/sys/fs/cgroup/{}", scope.unit));
        report.cpu_quota_us = Some(150_000);
        report.cpu_period_us = Some(100_000);
        report.memory_max_bytes = Some(100);
        report.memory_swap_max_bytes = Some(0);
        assert!(validate_report(&report, scope.cpus, scope.memory_bytes).is_ok());
        report.cpu_quota_us = None;
        assert!(validate_report(&report, scope.cpus, scope.memory_bytes).is_err());
        report.cpu_quota_us = Some(300_000);
        assert!(validate_report(&report, scope.cpus, scope.memory_bytes).is_err());
        report.cpu_quota_us = Some(150_000);
        report.memory_swap_max_bytes = Some(1);
        assert!(validate_report(&report, scope.cpus, scope.memory_bytes).is_err());
        report.cgroup = Some("/sys/fs/cgroup/another.scope".into());
        assert!(validate_report(&report, scope.cpus, None).is_err());
    }

    #[test]
    fn cgroup_and_quota_parsers_fail_on_ambiguous_or_malformed_state() {
        assert_eq!(
            cpu_limit("200000 100000\n").unwrap(),
            (Some(200000), 100000)
        );
        assert_eq!(cpu_limit("max 100000").unwrap(), (None, 100000));
        for invalid in ["200000", "1 0", "x 100000", "1 2 extra"] {
            assert!(cpu_limit(invalid).is_err());
        }
        assert!(scoped_cgroup("0::/user/a.scope\n", "a.scope").is_ok());
        for path in [
            "0::/user/../a.scope",
            "0::relative/a.scope",
            "1:cpu:/a.scope",
            "0::/other.scope",
        ] {
            assert!(scoped_cgroup(path, "a.scope").is_err());
        }
    }
}
