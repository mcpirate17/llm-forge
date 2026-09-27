//! Read-only resource admission. Snapshots are observations, not reservations;
//! CPU/RAM enforcement belongs to the task runner and GPU masks are cooperative.

use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::Serialize;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MIB: u64 = 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Args, Debug, Clone, Default, Serialize)]
pub struct ResourceRequest {
    /// Maximum CPU capacity for the child (at least 0.01); enforced by the runner.
    #[arg(long)]
    pub cpus: Option<f64>,
    /// Maximum memory in MiB for the child; enforced by the task runner.
    #[arg(long)]
    pub memory_mib: Option<u64>,
    /// Refuse admission when the host's one-minute load exceeds this value.
    #[arg(long)]
    pub max_load: Option<f64>,
    /// Explicit NVIDIA GPU index or full GPU UUID; default is CPU-only.
    #[arg(long)]
    pub gpu: Option<String>,
    /// Require this much currently free VRAM; this is not a GPU memory cap.
    #[arg(long, requires = "gpu")]
    pub vram_mib: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceSnapshot {
    pub captured_at: i64,
    pub allowed_cpus: usize,
    pub effective_cpus: f64,
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    pub memory_total_mib: u64,
    /// Minimum of host MemAvailable and all visible cgroup memory headrooms.
    pub memory_available_mib: u64,
    pub cgroup_v2_path: Option<String>,
    pub cgroup_memory_limit_mib: Option<u64>,
    pub cgroup_memory_available_mib: Option<u64>,
    pub gpu: Option<GpuSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuSnapshot {
    pub index: u32,
    pub uuid: String,
    pub memory_total_mib: u64,
    pub memory_free_mib: u64,
    pub utilization_percent: u32,
}

#[derive(Debug, Default)]
struct CgroupLimits {
    path: Option<String>,
    cpus: Option<f64>,
    memory_limit: Option<u64>,
    memory_available: Option<u64>,
}

impl ResourceRequest {
    pub fn validate(&self) -> Result<()> {
        if let Some(cpus) = self.cpus {
            ensure!(
                cpus.is_finite() && cpus >= 0.01,
                "--cpus must be finite and at least 0.01 CPUs"
            );
        }
        if let Some(memory) = self.memory_mib {
            ensure!(
                memory > 0 && memory.checked_mul(MIB).is_some(),
                "--memory-mib must be positive and fit in bytes"
            );
        }
        if let Some(load) = self.max_load {
            ensure!(
                load.is_finite() && load >= 0.0,
                "--max-load must be finite and nonnegative"
            );
        }
        if let Some(gpu) = &self.gpu {
            ensure!(
                gpu.parse::<u32>().is_ok() || valid_uuid(gpu),
                "--gpu must be an NVIDIA index or full GPU UUID"
            );
        }
        if let Some(vram) = self.vram_mib {
            ensure!(self.gpu.is_some(), "--vram-mib requires --gpu");
            ensure!(vram > 0, "--vram-mib must be positive");
        }
        Ok(())
    }

    pub fn admit(&self) -> Result<ResourceSnapshot> {
        self.validate()?;
        let mut snapshot = cpu_memory_snapshot()?;
        // Reject CPU/RAM requests before invoking any GPU management process.
        self.check_snapshot(&snapshot, false)?;
        if let Some(selection) = &self.gpu {
            let cards = parse_gpu_rows(&probe_nvidia()?)?;
            let card = select_gpu(&cards, selection)?;
            validate_visibility(&cards, card)?;
            snapshot.gpu = Some(card.clone());
        }
        self.check_snapshot(&snapshot, true)?;
        Ok(snapshot)
    }

    pub fn configure_command(
        &self,
        command: &mut Command,
        snapshot: &ResourceSnapshot,
    ) -> Result<()> {
        self.validate()?;
        self.check_snapshot(snapshot, true)?;
        match (&self.gpu, &snapshot.gpu) {
            (Some(selection), Some(card)) => {
                ensure!(
                    gpu_matches(card, selection),
                    "resource snapshot does not match --gpu"
                );
                command.env("CUDA_VISIBLE_DEVICES", &card.uuid);
                command.env("NVIDIA_VISIBLE_DEVICES", &card.uuid);
            }
            (None, None) => {
                command.env("CUDA_VISIBLE_DEVICES", "");
                command.env("NVIDIA_VISIBLE_DEVICES", "void");
            }
            _ => bail!("resource snapshot does not match GPU request"),
        }
        command.env("ROCR_VISIBLE_DEVICES", "");
        command.env("HIP_VISIBLE_DEVICES", "");
        Ok(())
    }

    fn check_snapshot(&self, snapshot: &ResourceSnapshot, check_gpu: bool) -> Result<()> {
        if let Some(cpus) = self.cpus {
            ensure!(
                cpus <= snapshot.effective_cpus,
                "requested {cpus} CPUs exceeds effective capacity {}",
                snapshot.effective_cpus
            );
        }
        if let Some(memory) = self.memory_mib {
            ensure!(
                memory <= snapshot.memory_available_mib,
                "requested {memory} MiB exceeds available RAM {} MiB",
                snapshot.memory_available_mib
            );
        }
        if let Some(load) = self.max_load {
            ensure!(
                snapshot.load_1m <= load,
                "one-minute load {} exceeds --max-load {load}",
                snapshot.load_1m
            );
        }
        if check_gpu && self.gpu.is_some() {
            let card = snapshot
                .gpu
                .as_ref()
                .context("GPU request has no admitted GPU snapshot")?;
            if let Some(vram) = self.vram_mib {
                ensure!(
                    vram <= card.memory_free_mib,
                    "requested {vram} MiB VRAM exceeds currently free {} MiB on {}",
                    card.memory_free_mib,
                    card.uuid
                );
            }
        }
        Ok(())
    }
}

fn cpu_memory_snapshot() -> Result<ResourceSnapshot> {
    ensure!(
        cfg!(target_os = "linux"),
        "resource admission requires Linux /proc"
    );
    let status = fs::read_to_string("/proc/self/status").context("reading allowed CPUs")?;
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .context("/proc/self/status has no Cpus_allowed_list")?;
    let allowed_cpus = parse_cpu_list(allowed.trim())?;
    let load = parse_load(&fs::read_to_string("/proc/loadavg").context("reading system load")?)?;
    let (total, available) =
        parse_meminfo(&fs::read_to_string("/proc/meminfo").context("reading available RAM")?)?;
    let cgroup = probe_cgroup_limits()?;
    Ok(ResourceSnapshot {
        captured_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs()
            .try_into()?,
        allowed_cpus,
        effective_cpus: cgroup
            .cpus
            .map_or(allowed_cpus as f64, |limit| limit.min(allowed_cpus as f64)),
        load_1m: load[0],
        load_5m: load[1],
        load_15m: load[2],
        memory_total_mib: total / MIB,
        memory_available_mib: available.min(cgroup.memory_available.unwrap_or(available)) / MIB,
        cgroup_v2_path: cgroup.path,
        cgroup_memory_limit_mib: cgroup.memory_limit.map(|value| value / MIB),
        cgroup_memory_available_mib: cgroup.memory_available.map(|value| value / MIB),
        gpu: None,
    })
}

fn parse_cpu_list(value: &str) -> Result<usize> {
    let mut total = 0usize;
    let mut previous = None;
    for segment in value.split(',') {
        let (start, end) = segment.split_once('-').unwrap_or((segment, segment));
        let start: u32 = start.parse().context("invalid allowed CPU index")?;
        let end: u32 = end.parse().context("invalid allowed CPU range")?;
        ensure!(
            start <= end && previous.is_none_or(|prior| start > prior),
            "allowed CPU ranges overlap or are unordered"
        );
        total = total
            .checked_add((u64::from(end) - u64::from(start) + 1).try_into()?)
            .context("CPU count overflow")?;
        previous = Some(end);
    }
    ensure!(total > 0, "no allowed CPUs");
    Ok(total)
}

fn parse_load(value: &str) -> Result<[f64; 3]> {
    let mut fields = value.split_whitespace();
    let mut loads = [0.0_f64; 3];
    for load in &mut loads {
        *load = fields.next().context("missing load average")?.parse()?;
        ensure!(load.is_finite() && *load >= 0.0, "invalid load average");
    }
    Ok(loads)
}

fn parse_meminfo(value: &str) -> Result<(u64, u64)> {
    let mut total = None;
    let mut available = None;
    for line in value.lines() {
        let Some((name, data)) = line.split_once(':') else {
            continue;
        };
        let target = match name {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            _ => continue,
        };
        ensure!(target.is_none(), "duplicate meminfo field {name}");
        let fields: Vec<_> = data.split_whitespace().collect();
        ensure!(
            fields.len() == 2 && fields[1] == "kB",
            "invalid meminfo field {name}"
        );
        *target = Some(
            fields[0]
                .parse::<u64>()?
                .checked_mul(1024)
                .context("meminfo overflow")?,
        );
    }
    let total = total.context("meminfo has no MemTotal")?;
    let available = available.context("meminfo has no MemAvailable")?;
    ensure!(total > 0 && available <= total, "invalid available RAM");
    Ok((total, available))
}

fn mount_path(value: &str) -> Result<PathBuf> {
    // mountinfo uses octal escapes for whitespace and backslashes in paths.
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            ensure!(i + 3 < raw.len(), "truncated mountinfo escape");
            let digits = std::str::from_utf8(&raw[i + 1..i + 4])?;
            bytes.push(u8::from_str_radix(digits, 8).context("invalid mountinfo escape")?);
            i += 4;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(String::from_utf8(bytes)?);
    ensure!(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "unsafe cgroup path"
    );
    Ok(path)
}

fn cgroup_location(membership: &str, mounts: &str) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(group) = membership.lines().find_map(|line| line.strip_prefix("0::")) else {
        return Ok(None);
    };
    let group = PathBuf::from(group);
    ensure!(
        group.is_absolute()
            && !group
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
        "unsafe cgroup membership"
    );
    for line in mounts.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        if right.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let fields: Vec<_> = left.split_whitespace().collect();
        ensure!(fields.len() >= 6, "malformed cgroup mountinfo");
        let root = mount_path(fields[3])?;
        let mount = mount_path(fields[4])?;
        let relative = group
            .strip_prefix(&root)
            .or_else(|_| group.strip_prefix("/"))?;
        return Ok(Some((mount.join(relative), mount)));
    }
    bail!("unified cgroup membership exists without a visible cgroup2 mount")
}

fn read_control(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        // Root groups and groups without an enabled controller have no max file.
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("reading cgroup control {}", path.display()))
        }
    }
}

fn parse_cpu_max(value: &str) -> Result<Option<f64>> {
    let fields: Vec<_> = value.split_whitespace().collect();
    ensure!(fields.len() == 2, "malformed cgroup cpu.max");
    let period: u64 = fields[1].parse().context("invalid cpu.max period")?;
    ensure!(period > 0, "zero cpu.max period");
    if fields[0] == "max" {
        return Ok(None);
    }
    let quota: u64 = fields[0].parse().context("invalid cpu.max quota")?;
    ensure!(quota > 0, "zero cpu.max quota");
    Ok(Some(quota as f64 / period as f64))
}

fn probe_cgroup_limits() -> Result<CgroupLimits> {
    let membership =
        fs::read_to_string("/proc/self/cgroup").context("reading cgroup membership")?;
    let mounts = fs::read_to_string("/proc/self/mountinfo").context("reading cgroup mounts")?;
    let Some((leaf, root)) = cgroup_location(&membership, &mounts)? else {
        return Ok(CgroupLimits::default());
    };
    let mut limits = CgroupLimits {
        path: Some(leaf.display().to_string()),
        ..Default::default()
    };
    let mut directory = leaf.as_path();
    loop {
        // Establish that this is a readable group before accepting absent controls.
        fs::read_to_string(directory.join("cgroup.controllers"))
            .with_context(|| format!("reading cgroup {}", directory.display()))?;
        if let Some(value) = read_control(&directory.join("cpu.max"))? {
            if let Some(cpus) = parse_cpu_max(&value)? {
                limits.cpus = Some(limits.cpus.map_or(cpus, |current| current.min(cpus)));
            }
        }
        if let Some(value) = read_control(&directory.join("memory.max"))? {
            if value.trim() != "max" {
                let maximum: u64 = value.trim().parse().context("invalid memory.max")?;
                let current: u64 = fs::read_to_string(directory.join("memory.current"))?
                    .trim()
                    .parse()
                    .context("invalid memory.current")?;
                let available = maximum.saturating_sub(current);
                limits.memory_limit = Some(
                    limits
                        .memory_limit
                        .map_or(maximum, |prior| prior.min(maximum)),
                );
                limits.memory_available = Some(
                    limits
                        .memory_available
                        .map_or(available, |prior| prior.min(available)),
                );
            }
        }
        if directory == root {
            break;
        }
        directory = directory
            .parent()
            .filter(|parent| parent.starts_with(&root))
            .context("cgroup escaped its mount")?;
    }
    Ok(limits)
}

fn valid_uuid(value: &str) -> bool {
    value.strip_prefix("GPU-").is_some_and(|suffix| {
        suffix.len() == 36
            && suffix.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
    })
}

fn parse_gpu_rows(value: &str) -> Result<Vec<GpuSnapshot>> {
    let mut cards: Vec<GpuSnapshot> = Vec::new();
    for line in value.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split(',').map(str::trim).collect();
        ensure!(fields.len() == 5, "unexpected nvidia-smi output row");
        let card = GpuSnapshot {
            index: fields[0].parse().context("invalid NVIDIA index")?,
            uuid: fields[1].to_owned(),
            memory_total_mib: fields[2].parse().context("invalid GPU total memory")?,
            memory_free_mib: fields[3].parse().context("invalid GPU free memory")?,
            utilization_percent: fields[4].parse().context("GPU utilization unavailable")?,
        };
        ensure!(valid_uuid(&card.uuid), "invalid NVIDIA GPU UUID");
        ensure!(
            card.memory_total_mib > 0
                && card.memory_free_mib <= card.memory_total_mib
                && card.utilization_percent <= 100,
            "invalid NVIDIA resource values"
        );
        ensure!(
            !cards
                .iter()
                .any(|prior| prior.index == card.index
                    || prior.uuid.eq_ignore_ascii_case(&card.uuid)),
            "duplicate NVIDIA GPU identity"
        );
        cards.push(card);
    }
    ensure!(!cards.is_empty(), "nvidia-smi reported no GPUs");
    Ok(cards)
}

fn gpu_matches(card: &GpuSnapshot, selection: &str) -> bool {
    selection
        .parse::<u32>()
        .is_ok_and(|index| card.index == index)
        || card.uuid.eq_ignore_ascii_case(selection)
}

fn select_gpu<'a>(cards: &'a [GpuSnapshot], selection: &str) -> Result<&'a GpuSnapshot> {
    cards
        .iter()
        .find(|card| gpu_matches(card, selection))
        .with_context(|| format!("NVIDIA GPU {selection:?} not found"))
}

fn check_mask(mask: &str, cards: &[GpuSnapshot], selected: &GpuSnapshot, cuda: bool) -> Result<()> {
    if !cuda && mask == "all" {
        return Ok(());
    }
    ensure!(
        !mask.is_empty() && mask != "-1" && mask != "none" && mask != "void",
        "inherited GPU visibility disables GPU access"
    );
    let mut allowed = false;
    for token in mask.split(',') {
        if let Ok(index) = token.parse::<u32>() {
            ensure!(!cuda, "inherited CUDA_VISIBLE_DEVICES uses ambiguous numeric ordinals; use GPU UUIDs to preserve device visibility");
            allowed |= selected.index == index;
            continue;
        }
        ensure!(
            token.starts_with("GPU-") && token.len() > 4,
            "unsupported inherited GPU visibility token {token:?}"
        );
        let matches: Vec<_> = cards
            .iter()
            .filter(|card| {
                card.uuid
                    .to_ascii_lowercase()
                    .starts_with(&token.to_ascii_lowercase())
            })
            .collect();
        ensure!(
            matches.len() == 1,
            "inherited GPU UUID prefix is unknown or ambiguous"
        );
        allowed |= matches[0].uuid == selected.uuid;
    }
    ensure!(
        allowed,
        "requested GPU {} is outside inherited visibility",
        selected.uuid
    );
    Ok(())
}

fn validate_visibility(cards: &[GpuSnapshot], selected: &GpuSnapshot) -> Result<()> {
    for (name, cuda) in [
        ("CUDA_VISIBLE_DEVICES", true),
        ("NVIDIA_VISIBLE_DEVICES", false),
    ] {
        if let Some(mask) = std::env::var_os(name) {
            check_mask(
                mask.to_str()
                    .with_context(|| format!("{name} is not UTF-8"))?,
                cards,
                selected,
                cuda,
            )
            .with_context(|| format!("preserving {name}"))?;
        }
    }
    Ok(())
}

struct ProbeChild(Child, bool);

/// Observe exit without reaping, keeping the PID pinned for process-group cleanup.
/// The caller must eventually reap with `Child::wait` after cleaning its group.
#[cfg(target_os = "linux")]
pub fn child_exit_status(child: &Child) -> Result<Option<ExitStatus>> {
    use std::os::unix::process::ExitStatusExt;
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // Child::try_wait would reap the leader and open a PID-reuse window before
    // the group kill. WNOWAIT preserves that identity until the caller reaps.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error).context("observing child exit without reaping");
    }
    let pid = unsafe { info.si_pid() };
    if pid == 0 {
        return Ok(None);
    }
    ensure!(
        pid == child.id() as libc::pid_t,
        "exit observer returned another child"
    );
    let status = unsafe { info.si_status() };
    let raw = match info.si_code {
        libc::CLD_EXITED => {
            ensure!((0..=255).contains(&status), "invalid observed exit code");
            status << 8
        }
        libc::CLD_KILLED | libc::CLD_DUMPED => {
            ensure!((1..=127).contains(&status), "invalid observed exit signal");
            status
                | if info.si_code == libc::CLD_DUMPED {
                    0x80
                } else {
                    0
                }
        }
        code => bail!("unexpected child exit observation code {code}"),
    };
    Ok(Some(ExitStatus::from_raw(raw)))
}

#[cfg(not(target_os = "linux"))]
pub fn child_exit_status(_child: &Child) -> Result<Option<ExitStatus>> {
    bail!("observing child exit without reaping requires Linux")
}

#[cfg(target_os = "linux")]
impl ProbeChild {
    fn finish(&mut self) -> Result<ExitStatus> {
        // The leader is exited but unreaped, so its identity is still pinned.
        // Terminate any descendants before reaping, even on normal completion.
        let result = unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("cleaning completed probe process group");
            }
        }
        let status = self.0.wait().context("reaping completed probe")?;
        self.1 = true;
        Ok(status)
    }
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        if self.1 {
            return;
        }
        // Only this probe's process group; never another task or model process.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn probe_nvidia() -> Result<String> {
    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-gpu=index,uuid,memory.total,memory.free,utilization.gpu",
        "--format=csv,noheader,nounits",
    ]);
    let output = run_probe(&mut command, PROBE_TIMEOUT)?;
    ensure!(
        output.status.success(),
        "nvidia-smi probe exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("GPU probe output is not UTF-8")
}

/// Bounded capture for management probes. Preserves caller-provided environment
/// and working directory; a nonzero exit is returned for the caller to interpret.
#[cfg(target_os = "linux")]
pub fn run_probe(command: &mut Command, timeout: Duration) -> Result<Output> {
    use std::os::unix::process::CommandExt;
    ensure!(!timeout.is_zero(), "probe timeout must be positive");
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("probe timeout is too large")?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = ProbeChild(
        command
            .spawn()
            .with_context(|| format!("starting probe {:?}", command.get_program()))?,
        false,
    );
    let mut stdout = child.0.stdout.take().context("probe stdout unavailable")?;
    let mut stderr = child.0.stderr.take().context("probe stderr unavailable")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        drain_probe(&mut stdout, &mut out, err.len())?;
        drain_probe(&mut stderr, &mut err, out.len())?;
        if child_exit_status(&child.0)?.is_some() {
            let status = child.finish()?;
            drain_probe(&mut stdout, &mut out, err.len())?;
            drain_probe(&mut stderr, &mut err, out.len())?;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        ensure!(
            Instant::now() < deadline,
            "probe {:?} timed out after {} ms",
            command.get_program(),
            timeout.as_millis()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn run_probe(_command: &mut Command, _timeout: Duration) -> Result<Output> {
    bail!("bounded resource probing requires Linux")
}

#[cfg(target_os = "linux")]
fn nonblocking(input: &impl std::os::fd::AsRawFd) -> Result<()> {
    let fd = input.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "setting probe pipe nonblocking: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

fn drain_probe(input: &mut impl Read, output: &mut Vec<u8>, other_bytes: usize) -> Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                ensure!(
                    output.len() + other_bytes + count <= PROBE_OUTPUT_LIMIT,
                    "probe output exceeds {PROBE_OUTPUT_LIMIT} bytes"
                );
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
            // Return to the outer deadline check even under repeated signals.
            Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(()),
            Err(error) => return Err(error).context("reading probe output"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    const FIRST: &str = "GPU-aaaaaaaa-1111-2222-3333-444444444444";
    const SECOND: &str = "GPU-bbbbbbbb-1111-2222-3333-444444444444";

    fn cards() -> Vec<GpuSnapshot> {
        parse_gpu_rows(&format!(
            "0, {FIRST}, 24000, 8000, 75\n1, {SECOND}, 48000, 40000, 0\n"
        ))
        .unwrap()
    }

    fn snapshot() -> ResourceSnapshot {
        ResourceSnapshot {
            captured_at: 1,
            allowed_cpus: 8,
            effective_cpus: 2.5,
            load_1m: 3.0,
            load_5m: 2.0,
            load_15m: 1.0,
            memory_total_mib: 32000,
            memory_available_mib: 1024,
            cgroup_v2_path: Some("/sys/fs/cgroup/example".into()),
            cgroup_memory_limit_mib: Some(4096),
            cgroup_memory_available_mib: Some(1024),
            gpu: None,
        }
    }

    #[test]
    fn request_validation_rejects_invalid_numbers_and_unbound_vram() {
        for cpus in [0.0, 0.009, -1.0, f64::NAN, f64::INFINITY] {
            assert!(ResourceRequest {
                cpus: Some(cpus),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        assert!(ResourceRequest {
            cpus: Some(0.01),
            ..Default::default()
        }
        .validate()
        .is_ok());
        for load in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(ResourceRequest {
                max_load: Some(load),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        for memory in [0, u64::MAX] {
            assert!(ResourceRequest {
                memory_mib: Some(memory),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        assert!(ResourceRequest {
            vram_mib: Some(1),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ResourceRequest {
            gpu: Some("all".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(ResourceRequest {
            cpus: Some(0.5),
            max_load: Some(0.0),
            gpu: Some(FIRST.into()),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn admission_enforces_requested_capacity_without_automatic_load_thresholds() {
        let snap = snapshot();
        assert!(ResourceRequest::default()
            .check_snapshot(&snap, true)
            .is_ok());
        assert!(ResourceRequest {
            cpus: Some(3.0),
            ..Default::default()
        }
        .check_snapshot(&snap, true)
        .is_err());
        assert!(ResourceRequest {
            memory_mib: Some(1025),
            ..Default::default()
        }
        .check_snapshot(&snap, true)
        .is_err());
        assert!(ResourceRequest {
            max_load: Some(2.9),
            ..Default::default()
        }
        .check_snapshot(&snap, true)
        .is_err());
        assert!(ResourceRequest {
            cpus: Some(2.5),
            memory_mib: Some(1024),
            max_load: Some(3.0),
            ..Default::default()
        }
        .check_snapshot(&snap, true)
        .is_ok());
    }

    #[test]
    fn admission_requires_requested_vram_without_claiming_a_reservation() {
        let mut snap = snapshot();
        snap.gpu = Some(cards()[0].clone());
        let mut request = ResourceRequest {
            gpu: Some("0".into()),
            vram_mib: Some(8001),
            ..Default::default()
        };
        assert!(request.check_snapshot(&snap, true).is_err());
        request.vram_mib = Some(8000);
        assert!(request.check_snapshot(&snap, true).is_ok());
        snap.gpu = None;
        assert!(request.check_snapshot(&snap, true).is_err());
    }

    #[test]
    fn cpu_only_command_hides_all_gpu_runtimes() {
        let mut command = Command::new("unused");
        ResourceRequest::default()
            .configure_command(&mut command, &snapshot())
            .unwrap();
        let env: BTreeMap<_, _> = command
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.unwrap().to_str().unwrap()))
            .collect();
        assert_eq!(env["CUDA_VISIBLE_DEVICES"], "");
        assert_eq!(env["NVIDIA_VISIBLE_DEVICES"], "void");
        assert_eq!(env["ROCR_VISIBLE_DEVICES"], "");
        assert_eq!(env["HIP_VISIBLE_DEVICES"], "");
    }

    #[test]
    fn gpu_command_uses_canonical_identity_and_refuses_mismatched_snapshots() {
        let mut command = Command::new("unused");
        let mut snap = snapshot();
        snap.gpu = Some(cards()[1].clone());
        let request = ResourceRequest {
            gpu: Some("1".into()),
            ..Default::default()
        };
        request.configure_command(&mut command, &snap).unwrap();
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "CUDA_VISIBLE_DEVICES")
                .unwrap()
                .1
                .unwrap(),
            SECOND
        );
        snap.gpu = Some(cards()[0].clone());
        assert!(request.configure_command(&mut command, &snap).is_err());
        assert!(ResourceRequest::default()
            .configure_command(&mut command, &snap)
            .is_err());
    }

    #[test]
    fn inherited_masks_never_expand_visibility_or_guess_cuda_order() {
        let devices = cards();
        for mask in ["", "-1", "0", SECOND, "all", "MIG-example", "GPU-"] {
            assert!(
                check_mask(mask, &devices, &devices[0], true).is_err(),
                "{mask}"
            );
        }
        assert!(check_mask(FIRST, &devices, &devices[0], true).is_ok());
        assert!(check_mask("GPU-aaaa", &devices, &devices[0], true).is_ok());
        assert!(check_mask("all", &devices, &devices[0], false).is_ok());
        assert!(check_mask("0", &devices, &devices[0], false).is_ok());
        assert!(check_mask("1", &devices, &devices[0], false).is_err());
        let mut ambiguous = devices.clone();
        ambiguous[1].uuid = "GPU-aaaabbbb-1111-2222-3333-444444444444".into();
        assert!(check_mask("GPU-aaaa", &ambiguous, &ambiguous[0], true).is_err());
    }

    #[test]
    fn gpu_parser_rejects_unavailable_or_inconsistent_telemetry() {
        for row in [
            format!("0, {FIRST}, 10, 11, 0"),
            format!("0, {FIRST}, 10, 9, N/A"),
            format!("0, {FIRST}, 10, 9, 101"),
            "".into(),
            "0, bad, 10, 9, 0".into(),
        ] {
            assert!(parse_gpu_rows(&row).is_err());
        }
        assert!(parse_gpu_rows(&format!("0, {FIRST}, 10, 9, 0\n0, {SECOND}, 10, 9, 0")).is_err());
        assert_eq!(select_gpu(&cards(), "1").unwrap().uuid, SECOND);
        assert!(select_gpu(&cards(), "2").is_err());
    }

    #[test]
    fn proc_parsers_preserve_units_and_reject_bad_constraints() {
        assert_eq!(parse_cpu_list("0-3,8,10-11").unwrap(), 7);
        for value in ["", "3-1", "0-3,2", "1,,2"] {
            assert!(parse_cpu_list(value).is_err());
        }
        assert_eq!(
            parse_load("1.25 2.5 3.75 1/10 123").unwrap(),
            [1.25, 2.5, 3.75]
        );
        for value in ["NaN 0 0", "-1 0 0", "1 2"] {
            assert!(parse_load(value).is_err());
        }
        assert_eq!(
            parse_meminfo("MemTotal: 2048 kB\nMemAvailable: 1024 kB\n").unwrap(),
            (2 * MIB, MIB)
        );
        for value in [
            "MemTotal: 1 kB",
            "MemTotal: 1 kB\nMemAvailable: 2 kB",
            "MemTotal: 2 MB\nMemAvailable: 1 kB",
        ] {
            assert!(parse_meminfo(value).is_err());
        }
        assert_eq!(parse_cpu_max("250000 100000").unwrap(), Some(2.5));
        assert_eq!(parse_cpu_max("max 100000").unwrap(), None);
        for value in ["max", "max 0", "0 100", "a 100", "100 100 extra"] {
            assert!(parse_cpu_max(value).is_err());
        }
    }

    #[test]
    fn cgroup_mount_resolution_handles_namespace_roots_and_rejects_escape() {
        let mount = "25 24 0:22 / /sys/fs/cgroup rw - cgroup2 cgroup rw";
        assert_eq!(
            cgroup_location("0::/user.slice/job\n", mount).unwrap(),
            Some((
                PathBuf::from("/sys/fs/cgroup/user.slice/job"),
                PathBuf::from("/sys/fs/cgroup")
            ))
        );
        assert!(cgroup_location("0::/../escape", mount).is_err());
        assert!(cgroup_location("0::/job", "").is_err());
        assert!(cgroup_location("2:cpu:/job", mount).unwrap().is_none());
        let namespaced = "25 24 0:22 /container /sys/fs/cgroup rw - cgroup2 cgroup rw";
        assert_eq!(
            cgroup_location("0::/", namespaced).unwrap().unwrap().0,
            PathBuf::from("/sys/fs/cgroup")
        );
        assert_eq!(mount_path("/a\\040b").unwrap(), PathBuf::from("/a b"));
    }

    #[test]
    fn probe_output_cap_includes_both_streams() {
        let mut out = Vec::new();
        drain_probe(&mut Cursor::new(b"ok"), &mut out, 0).unwrap();
        assert_eq!(out, b"ok");
        assert!(drain_probe(&mut Cursor::new(vec![0; PROBE_OUTPUT_LIMIT]), &mut out, 1).is_err());
        assert!(out.len() <= PROBE_OUTPUT_LIMIT);
    }
}
