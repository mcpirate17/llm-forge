//! Strict Linux filesystem, signal-domain and syscall confinement.
//!
//! Policy is supplied by the operator, never by model tool arguments. Preparation
//! opens every requested capability and fails on missing paths or kernel support.
//! Only signal-safe syscalls execute in the child between fork and exec.
#[path = "sandbox_broker.rs"]
mod broker;
#[path = "sandbox_filter.rs"]
mod filter;
#[path = "sandbox_metadata.rs"]
mod metadata;
#[path = "sandbox_process.rs"]
mod process;
#[path = "sandbox_transport.rs"]
mod transport;
pub use process::{CommandPlan, ConfinedProcess};

use anyhow::{ensure, Context, Result};
use landlock::{
    Access, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreated, RulesetCreatedAttr, Scope, ABI,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRED_ABI: i64 = 6;

pub struct Policy {
    pub read_only: Vec<PathBuf>,
    /// An existing empty directory owned by this user with mode 0700. All
    /// HOME/cache/temp writes stay here; no pre-existing hardlinks are admitted.
    pub scratch: PathBuf,
    pub working_directory: PathBuf,
    /// Explicit non-secret environment only; the parent environment is cleared.
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub backend: &'static str,
    pub landlock_abi: i64,
    pub required_abi: i64,
    pub read_only: Vec<PathBuf>,
    pub scratch: PathBuf,
    pub working_directory: PathBuf,
    pub network: &'static str,
    pub file_metadata_changes: &'static str,
}

pub struct Sandbox {
    ruleset: OwnedFd,
    report: Report,
    environment: BTreeMap<String, String>,
    scratch_root: cap_std::fs::Dir,
}

impl Sandbox {
    pub fn prepare(policy: Policy) -> Result<Self> {
        ensure!(
            policy.read_only.len() <= 128,
            "at most 128 read capabilities"
        );
        validate_environment(&policy.environment)?;
        let abi = kernel_abi()?;
        broker::check_abi()?;
        ensure!(
            abi >= REQUIRED_ABI,
            "Landlock ABI 6 is required; found {abi}"
        );
        let scratch = private_scratch(&policy.scratch)?;
        let working_directory = absolute(&policy.working_directory)?;
        ensure!(
            working_directory.is_dir(),
            "working directory must be a directory"
        );
        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V6))?
            .scope(Scope::from_all(ABI::V6))?
            .create()
            .context("create strict Landlock ruleset")?;
        let mut read_only = Vec::with_capacity(policy.read_only.len());
        for path in policy.read_only {
            let path = absolute(&path)?;
            ensure!(
                !path.starts_with(&scratch),
                "read-only capability lies in writable scratch"
            );
            add_path(&mut ruleset, &path, AccessFs::from_read(ABI::V6))?;
            read_only.push(path);
        }
        ensure!(
            working_directory.starts_with(&scratch)
                || read_only.iter().any(|p| working_directory.starts_with(p)),
            "working directory must be covered by a capability"
        );
        add_path(&mut ruleset, &scratch, writable_access())?;
        // Fixed device capabilities: no arbitrary device nodes or ioctl access.
        add_path(
            &mut ruleset,
            Path::new("/dev/null"),
            AccessFs::ReadFile | AccessFs::WriteFile,
        )?;
        add_path(
            &mut ruleset,
            Path::new("/dev/urandom"),
            AccessFs::ReadFile.into(),
        )?;
        let ruleset: Option<OwnedFd> = ruleset.into();
        Ok(Self {
            scratch_root: cap_std::fs::Dir::open_ambient_dir(&scratch, cap_std::ambient_authority())?,
            ruleset: ruleset.context("Landlock returned no enforcement descriptor")?,
            report: Report {
                backend: "landlock-seccomp-v1",
                landlock_abi: abi,
                required_abi: REQUIRED_ABI,
                read_only,
                scratch,
                working_directory,
                network: "denied; anonymous AF_UNIX socketpairs permitted",
                file_metadata_changes: "brokered chmod for owned scratch files/directories; other metadata changes denied",
            },
            environment: policy.environment,
        })
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// Builds a confined command. Spawn errors include failed enforcement; there
    /// is no fallback. Set stdio explicitly before spawning. Additional inherited
    /// descriptors are closed on exec, including descriptors lacking CLOEXEC.
    pub fn command(&self, program: &Path) -> Result<CommandPlan> {
        ensure!(
            program.is_absolute(),
            "sandbox program must be an absolute path"
        );
        let mut command = Command::new(program);
        command
            .current_dir(&self.report.working_directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &self.report.scratch)
            .env("TMPDIR", &self.report.scratch)
            .env("XDG_CACHE_HOME", &self.report.scratch)
            .envs(&self.environment);
        let ruleset = self.ruleset.try_clone()?;
        let instructions = filter::instructions()?;
        let (receiver, sender) = UnixDatagram::pair()?;
        // SAFETY: the closure uses only syscalls and preallocated immutable state.
        // The owned ruleset/filter remain live through spawn, including its fork.
        unsafe {
            command.pre_exec(move || {
                let core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
                    || libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) != 0
                    || libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32)
                        != 0
                {
                    return Err(io::Error::last_os_error());
                }
                let listener = filter::install(&instructions)?;
                let sent = transport::send(sender.as_raw_fd(), listener);
                libc::close(listener);
                sent
            });
        }
        Ok(CommandPlan {
            command,
            receiver,
            root: self.scratch_root.try_clone()?,
            scratch: self.report.scratch.clone(),
        })
    }
}

pub fn kernel_abi() -> Result<i64> {
    // SAFETY: query accepts a null attributes pointer and zero size.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            1u32,
        )
    };
    if abi < 0 {
        return Err(io::Error::last_os_error()).context("query Landlock ABI");
    }
    Ok(abi)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "sandbox capability must be absolute: {}",
        path.display()
    );
    path.canonicalize()
        .with_context(|| format!("resolve sandbox capability {}", path.display()))
}

fn private_scratch(path: &Path) -> Result<PathBuf> {
    let path = absolute(path)?;
    ensure!(
        path.parent().is_some(),
        "scratch must not be filesystem root"
    );
    let metadata = path.metadata()?;
    // SAFETY: geteuid only reads the caller's effective identity.
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o700,
        "scratch must be an owned mode-0700 directory"
    );
    ensure!(
        std::fs::read_dir(&path)?.next().transpose()?.is_none(),
        "scratch must be empty at preparation; existing hardlinks are unsafe"
    );
    Ok(path)
}

fn add_path(
    ruleset: &mut RulesetCreated,
    path: &Path,
    mut access: BitFlags<AccessFs>,
) -> Result<()> {
    let fd =
        PathFd::new(path).with_context(|| format!("open sandbox capability {}", path.display()))?;
    let metadata = File::from(fd.as_fd().try_clone_to_owned()?).metadata()?;
    if !metadata.is_dir() {
        access &= AccessFs::from_file(ABI::V6);
    }
    ruleset
        .add_rule(PathBeneath::new(fd, access))
        .with_context(|| format!("add sandbox capability {}", path.display()))?;
    Ok(())
}

fn writable_access() -> BitFlags<AccessFs> {
    AccessFs::from_read(ABI::V6)
        | AccessFs::WriteFile
        | AccessFs::Truncate
        | AccessFs::RemoveDir
        | AccessFs::RemoveFile
        | AccessFs::MakeDir
        | AccessFs::MakeReg
        | AccessFs::MakeSym
        | AccessFs::MakeFifo
        | AccessFs::Refer
}

pub(crate) fn validate_environment(environment: &BTreeMap<String, String>) -> Result<()> {
    ensure!(environment.len() <= 64, "at most 64 environment entries");
    for (key, value) in environment {
        ensure!(
            !key.is_empty()
                && key.len() <= 128
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && value.len() <= 8192
                && !value.contains('\0'),
            "invalid sandbox environment entry"
        );
        ensure!(
            !matches!(key.as_str(), "HOME" | "TMPDIR" | "XDG_CACHE_HOME"),
            "sandbox scratch environment cannot be overridden"
        );
    }
    Ok(())
}
