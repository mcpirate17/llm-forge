//! Owned cgroup v2 children under an explicitly delegated parent. Never changes
//! parent controllers/limits, adopts an existing group, or removes another group.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;
use tokio::io::{unix::AsyncFd, Interest};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub cpu_millis: u32,
    pub memory_mib: u64,
    pub processes: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub backend: &'static str,
    pub path: PathBuf,
    pub cpu_quota_us: u64,
    pub cpu_period_us: u64,
    pub memory_max_bytes: u64,
    pub memory_swap_max_bytes: u64,
    pub processes_max: u32,
    pub verified: bool,
}

pub struct Cgroup {
    parent: Dir,
    directory: Dir,
    name: String,
    report: Report,
    configured: bool,
    removed: bool,
}

impl Cgroup {
    pub fn create(parent_path: &Path, limits: Limits) -> Result<Self> {
        ensure!(
            (10..=1_024_000).contains(&limits.cpu_millis),
            "CPU budget must be 10..1024000 millicpus"
        );
        ensure!(
            (16..=1_048_576).contains(&limits.memory_mib),
            "memory budget must be 16..1048576 MiB"
        );
        ensure!(
            (1..=4096).contains(&limits.processes),
            "process budget must be 1..4096"
        );
        let parent_path = parent_path.canonicalize()?;
        let parent = delegated_parent(&parent_path)?;
        let name = crate::native_id::new("forge-native")?;
        parent
            .create_dir(&name)
            .context("create owned delegated cgroup")?;
        let directory = match parent.open_dir(&name) {
            Ok(directory) => directory,
            Err(error) => {
                if let Err(cleanup) = parent.remove_dir(&name) {
                    eprintln!("forge: cgroup preparation cleanup failed: {cleanup}");
                }
                return Err(error).context("open new delegated cgroup");
            }
        };
        let mut group = Self {
            parent,
            directory,
            name: name.clone(),
            configured: false,
            removed: false,
            report: Report {
                backend: "delegated-cgroup-v2",
                path: parent_path.join(name),
                cpu_quota_us: limits.cpu_millis as u64 * 100,
                cpu_period_us: 100_000,
                memory_max_bytes: limits.memory_mib * 1024 * 1024,
                memory_swap_max_bytes: 0,
                processes_max: limits.processes,
                verified: false,
            },
        };
        group.configure()?;
        Ok(group)
    }

    fn configure(&mut self) -> Result<()> {
        let entries = [
            (
                "cpu.max",
                format!("{} {}", self.report.cpu_quota_us, self.report.cpu_period_us),
            ),
            ("memory.max", self.report.memory_max_bytes.to_string()),
            ("memory.swap.max", "0".into()),
            ("memory.oom.group", "1".into()),
            ("pids.max", self.report.processes_max.to_string()),
        ];
        for (name, value) in entries {
            write(&self.directory, name, value.as_bytes())?;
            ensure!(
                self.directory.read_to_string(name)?.trim() == value,
                "cgroup controller readback differs for {name}"
            );
        }
        ensure!(
            self.directory.read_to_string("cgroup.type")?.trim() == "domain",
            "cgroup must be a valid domain"
        );
        self.report.verified = true;
        Ok(())
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// Attach in the trusted pre-exec phase, before any payload can fork. The
    /// preopened descriptor remains usable after Landlock and closes on exec.
    pub fn configure_command(&mut self, command: &mut Command) -> Result<()> {
        ensure!(
            !self.configured && !self.removed,
            "cgroup launch already configured or removed"
        );
        let membership = write_file(&self.directory, "cgroup.procs")?;
        // SAFETY: only a write syscall and scalar state are used after fork.
        unsafe {
            command.pre_exec(move || loop {
                let written = libc::write(membership.as_raw_fd(), b"0".as_ptr().cast(), 1);
                if written == 1 {
                    return Ok(());
                }
                let error = if written < 0 {
                    io::Error::last_os_error()
                } else {
                    io::Error::from_raw_os_error(libc::EIO)
                };
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            });
        }
        self.configured = true;
        Ok(())
    }

    pub fn verify_member(&self, child: &Child) -> Result<()> {
        let text = std::fs::read_to_string(format!("/proc/{}/cgroup", child.id()))?;
        let relative = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .context("unified cgroup entry missing")?;
        let expected = self
            .report
            .path
            .strip_prefix("/sys/fs/cgroup")
            .context("cgroup mount must be /sys/fs/cgroup")?;
        ensure!(
            Path::new(relative) == Path::new("/").join(expected),
            "child did not enter its owned cgroup"
        );
        Ok(())
    }

    pub fn kill(&self) -> Result<()> {
        if !self.removed {
            write(&self.directory, "cgroup.kill", b"1")?;
        }
        Ok(())
    }

    /// Deadline-bound cleanup uses cgroup.events priority notifications, with no
    /// timer polling. Requires a Tokio reactor; call before reaping process groups.
    pub async fn finish(&mut self, deadline: Duration) -> Result<()> {
        if self.removed {
            return Ok(());
        }
        ensure!(
            !deadline.is_zero() && deadline <= Duration::from_secs(30),
            "cleanup deadline must be (0,30s]"
        );
        let file = self.directory.open("cgroup.events")?.into_std();
        // SAFETY: AsyncFd owns the opened File throughout this wait. Reads borrow
        // that same File and no operation replaces or closes its descriptor.
        let events = unsafe { AsyncFd::register_with_interest(file, Interest::PRIORITY)? };
        self.kill()?;
        tokio::time::timeout(deadline, async {
            while populated(events.get_ref())? {
                events.ready(Interest::PRIORITY).await?.clear_ready();
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("cgroup cleanup deadline exceeded")??;
        self.parent
            .remove_dir(&self.name)
            .context("remove empty owned cgroup")?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        if let Err(error) = self.kill() {
            eprintln!("forge: cgroup emergency kill failed: {error:#}");
        }
        if let Err(error) = self.parent.remove_dir(&self.name) {
            eprintln!(
                "forge: cgroup {} requires cleanup: {error}",
                self.report.path.display()
            );
        }
    }
}

fn delegated_parent(path: &Path) -> Result<Dir> {
    ensure!(
        path.starts_with("/sys/fs/cgroup") && path != Path::new("/sys/fs/cgroup"),
        "delegation must be beneath /sys/fs/cgroup"
    );
    let directory = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
    let metadata = directory.try_clone()?.into_std_file().metadata()?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "delegated cgroup parent must be owned by this UID"
    );
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut filesystem) } != 0 {
        return Err(io::Error::last_os_error()).context("inspect delegated cgroup filesystem");
    }
    ensure!(
        filesystem.f_type == 0x63677270,
        "delegation is not cgroup v2"
    );
    let controllers = directory.read_to_string("cgroup.subtree_control")?;
    for name in ["cpu", "memory", "pids"] {
        ensure!(
            controllers.split_whitespace().any(|c| c == name),
            "delegated parent lacks enabled {name} controller"
        );
    }
    Ok(directory)
}

fn write_file(directory: &Dir, name: &str) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    Ok(directory.open_with(name, &options)?.into_std())
}

fn write(directory: &Dir, name: &str, value: &[u8]) -> Result<()> {
    use std::io::Write;
    write_file(directory, name)?
        .write_all(value)
        .with_context(|| format!("write cgroup controller {name}"))
}

fn populated(file: &File) -> Result<bool> {
    let mut buffer = [0u8; 4096];
    let count = file.read_at(&mut buffer, 0)?;
    ensure!(count < buffer.len(), "cgroup.events exceeds bound");
    let text = std::str::from_utf8(&buffer[..count])?;
    match text
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
    {
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => anyhow::bail!("invalid populated state in cgroup.events"),
    }
}
