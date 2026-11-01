//! A transient native daemon whose entire cgroup subtree follows its main PID.
//! The service manager, outside the daemon, handles abrupt supervisor death.
use crate::cgroup::Limits;
use crate::systemd::Manager;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub unit: String,
    pub lifetime_seconds: u64,
    pub cpu_millis: u32,
    pub memory_mib: u64,
    pub processes: u32,
}

pub struct Service {
    manager: Manager,
    record: Record,
    owned: bool,
}

impl Service {
    pub fn launch(
        program: &Path,
        arguments: &[OsString],
        limits: Limits,
        lifetime_seconds: u64,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self> {
        ensure!(program.is_absolute(), "service program must be absolute");
        ensure!(
            (1..=86400).contains(&lifetime_seconds),
            "service lifetime must be 1..86400 seconds"
        );
        ensure!(
            (10..=1_024_000).contains(&limits.cpu_millis)
                && (64..=1_048_576).contains(&limits.memory_mib)
                && (8..=4096).contains(&limits.processes),
            "invalid aggregate service budget"
        );
        crate::task_resources::ResourceRequest {
            cpus: Some(limits.cpu_millis as f64 / 1000.0),
            memory_mib: Some(limits.memory_mib),
            ..Default::default()
        }
        .admit()?;
        let manager = Manager::connect(254)?;
        let unit = format!("{}.service", crate::native_id::new("forge-harness")?);
        let mut command = launch_command(
            program,
            arguments,
            &unit,
            limits,
            lifetime_seconds,
            environment,
        )?;
        command.envs(manager.environment().iter().cloned());
        let service = Self {
            manager,
            record: Record {
                unit,
                lifetime_seconds,
                cpu_millis: limits.cpu_millis,
                memory_mib: limits.memory_mib,
                processes: limits.processes,
            },
            owned: true,
        };
        service
            .manager
            .successful(&mut command)
            .with_context(|| format!("launching owned service {}", service.record.unit))?;
        Ok(service)
    }

    pub fn record(&self) -> &Record {
        &self.record
    }

    /// Only an explicit user-facing start command should relinquish RAII ownership.
    pub fn detach(mut self) -> Record {
        self.owned = false;
        self.record.clone()
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.owned {
            return Ok(());
        }
        if let Err(error) = self.manager.successful_for(
            &mut self.manager.control(&["stop", &self.record.unit]),
            Duration::from_secs(8),
        ) {
            // --collect can remove a completed unit at any point. Verify absence
            // after a failed stop instead of racing a before-stop existence check.
            if !properties(&self.manager, &self.record.unit, &["LoadState"])?
                .get("LoadState")
                .is_some_and(|s| s == "not-found")
            {
                return Err(error);
            }
        }
        self.owned = false;
        Ok(())
    }

    /// Primarily for lifecycle verification: target the daemon main process, so
    /// the service manager must clean descendants rather than this caller.
    pub fn kill_main(&self) -> Result<()> {
        self.manager.successful(&mut self.manager.control(&[
            "kill",
            "--kill-whom=main",
            "--signal=KILL",
            &self.record.unit,
        ]))?;
        Ok(())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!(
                "forge: service {} cleanup failed: {error:#}",
                self.record.unit
            );
        }
    }
}

fn launch_command(
    program: &Path,
    arguments: &[OsString],
    unit: &str,
    limits: Limits,
    lifetime: u64,
    environment: &BTreeMap<String, String>,
) -> Result<Command> {
    ensure!(
        environment.len() <= 64,
        "too many daemon environment entries"
    );
    let mut command = Command::new("systemd-run");
    command
        .args([
            "--user",
            "--quiet",
            "--no-ask-password",
            "--collect",
            "--expand-environment=no",
            "--working-directory=/",
        ])
        .arg(format!("--unit={unit}"));
    for setting in [
        "Type=exec",
        "ExitType=main",
        "KillMode=control-group",
        "SendSIGKILL=yes",
        "TimeoutStopSec=5s",
        "TimeoutStartSec=5s",
        "Restart=no",
        "Delegate=yes",
        "DelegateSubgroup=supervisor",
        "UMask=0077",
        "CPUQuotaPeriodSec=100ms",
        "MemorySwapMax=0",
    ] {
        command.arg(format!("--property={setting}"));
    }
    command
        .arg(format!("--property=RuntimeMaxSec={lifetime}s"))
        .arg(format!(
            "--property=CPUQuota={}.{}%",
            limits.cpu_millis / 10,
            limits.cpu_millis % 10
        ))
        .arg(format!(
            "--property=MemoryMax={}",
            limits.memory_mib * 1024 * 1024
        ))
        .arg(format!("--property=TasksMax={}", limits.processes))
        .arg(format!("--setenv=FORGE_SERVICE_UNIT={unit}"));
    for (key, value) in environment {
        ensure!(
            !key.is_empty()
                && key.len() <= 128
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && value.len() <= 8192
                && !value.contains('\0')
                && key != "FORGE_SERVICE_UNIT",
            "invalid daemon environment entry"
        );
        command.arg(format!("--setenv={key}={value}"));
    }
    command.arg("--").arg(program).args(arguments);
    Ok(command)
}

/// Verify the running daemon's manager identity and cleanup policy before it
/// accepts commands. Returns its delegated root, not a caller-selected cgroup.
pub fn verify_self(unit: &str) -> Result<PathBuf> {
    validate_unit(unit)?;
    let manager = Manager::connect(254)?;
    let values = properties(
        &manager,
        unit,
        &[
            "MainPID",
            "ControlGroup",
            "Type",
            "ExitType",
            "KillMode",
            "SendSIGKILL",
            "Restart",
            "Delegate",
            "DelegateSubgroup",
            "RuntimeMaxUSec",
            "TimeoutStopUSec",
        ],
    )?;
    ensure!(
        values.get("MainPID").and_then(|v| v.parse::<u32>().ok()) == Some(std::process::id()),
        "service manager does not own this daemon PID"
    );
    for (key, expected) in [
        ("Type", "exec"),
        ("ExitType", "main"),
        ("KillMode", "control-group"),
        ("SendSIGKILL", "yes"),
        ("Restart", "no"),
        ("Delegate", "yes"),
        ("DelegateSubgroup", "supervisor"),
    ] {
        ensure!(
            values.get(key).is_some_and(|v| v == expected),
            "service {key} must be {expected}"
        );
    }
    let lifetime = duration(
        values
            .get("RuntimeMaxUSec")
            .context("missing runtime bound")?,
    )?;
    let stop = duration(
        values
            .get("TimeoutStopUSec")
            .context("missing stop bound")?,
    )?;
    ensure!(
        !lifetime.is_zero()
            && lifetime <= Duration::from_secs(86400)
            && !stop.is_zero()
            && stop <= Duration::from_secs(5),
        "service lifecycle bounds are invalid"
    );
    let relative = Path::new(
        values
            .get("ControlGroup")
            .context("missing service cgroup")?,
    );
    ensure!(
        relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && relative.file_name().is_some_and(|v| v == unit),
        "invalid owned service cgroup"
    );
    let current = std::fs::read_to_string("/proc/self/cgroup")?;
    let current = current
        .lines()
        .find_map(|s| s.strip_prefix("0::"))
        .context("unified cgroup required")?;
    ensure!(
        Path::new(current) == relative.join("supervisor"),
        "daemon is outside its delegated supervisor subgroup"
    );
    Ok(Path::new("/sys/fs/cgroup").join(relative.strip_prefix("/")?))
}

/// Enabling controllers is limited to this verified, newly owned service root.
/// The parent slice and other services are never modified here.
pub fn prepare_children(unit: &str) -> Result<PathBuf> {
    let root = verify_self(unit)?;
    ensure!(
        std::fs::read_to_string(root.join("cgroup.procs"))?
            .trim()
            .is_empty(),
        "service root contains processes"
    );
    std::fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids")?;
    let enabled = std::fs::read_to_string(root.join("cgroup.subtree_control"))?;
    ensure!(
        ["cpu", "memory", "pids"]
            .iter()
            .all(|name| enabled.split_whitespace().any(|v| v == *name)),
        "service delegation is incomplete"
    );
    Ok(root)
}

pub fn verify_limits(root: &Path, limits: Limits) -> Result<()> {
    for (name, expected) in [
        (
            "cpu.max",
            format!("{} 100000", u64::from(limits.cpu_millis) * 100),
        ),
        ("memory.max", (limits.memory_mib * 1024 * 1024).to_string()),
        ("memory.swap.max", "0".to_owned()),
        ("pids.max", limits.processes.to_string()),
    ] {
        ensure!(
            std::fs::read_to_string(root.join(name))?.trim() == expected,
            "aggregate service controller {name} differs from policy"
        );
    }
    Ok(())
}

fn properties(manager: &Manager, unit: &str, names: &[&str]) -> Result<BTreeMap<String, String>> {
    validate_unit(unit)?;
    let mut command = manager.control(&["show"]);
    command
        .arg(format!("--property={}", names.join(",")))
        .arg(unit);
    let output = crate::process_probe::run(&mut command, Duration::from_secs(3), 65536)?;
    let mut values = BTreeMap::new();
    for line in std::str::from_utf8(&output.stdout)?.lines() {
        let (key, value) = line
            .split_once('=')
            .context("invalid service-manager property")?;
        ensure!(
            values.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate service-manager property"
        );
    }
    ensure!(
        output.status.success() || values.get("LoadState").is_some_and(|s| s == "not-found"),
        "service query failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(values)
}

fn validate_unit(unit: &str) -> Result<()> {
    ensure!(
        unit.starts_with("forge-harness-")
            && unit.ends_with(".service")
            && unit.len() <= 128
            && unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
        "invalid owned harness service name"
    );
    Ok(())
}

fn duration(text: &str) -> Result<Duration> {
    let mut total = 0u64;
    for part in text.split_whitespace() {
        let (digits, scale) = [
            ("us", 1),
            ("µs", 1),
            ("ms", 1000),
            ("min", 60_000_000),
            ("h", 3_600_000_000),
            ("d", 86_400_000_000),
            ("s", 1_000_000),
        ]
        .into_iter()
        .find_map(|(suffix, scale)| part.strip_suffix(suffix).map(|digits| (digits, scale)))
        .context("unsupported service duration")?;
        total = total
            .checked_add(
                digits
                    .parse::<u64>()?
                    .checked_mul(scale)
                    .context("service duration overflow")?,
            )
            .context("service duration overflow")?;
    }
    ensure!(!text.is_empty(), "empty service duration");
    Ok(Duration::from_micros(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_and_unit_validation_reject_unbounded_or_ambiguous_values() {
        assert_eq!(duration("1min 40s").unwrap(), Duration::from_secs(100));
        assert_eq!(duration("50ms").unwrap(), Duration::from_millis(50));
        for value in ["", "infinity", "1.5s", "999999999999999999d"] {
            assert!(duration(value).is_err());
        }
        for unit in [
            "other.service",
            "forge-harness-../other.service",
            "forge-harness-abc.service extra",
        ] {
            assert!(validate_unit(unit).is_err());
        }
    }
}
