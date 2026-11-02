//! Bound libgit2's complete revision/packed-object work, not just final object sizes.
use super::{measure, VerifyArgs};
use crate::{
    confined_execution::{self, Limits},
    process_probe,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    process::Command,
    time::Duration,
};
const MEMORY: u64 = 2 * 1024 * 1024 * 1024;
const CPU: u64 = 30;
const WALL: u64 = 60;
const FRAME: usize = 16 * 1024 * 1024 + 4096;
const DIAGNOSTICS: usize = 64 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kernel {
    cpu_seconds: u64,
    address_space_bytes: u64,
    file_bytes: u64,
    core_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    schema_version: u8,
    limits: Kernel,
    verdict: Option<Value>,
    error: Option<String>,
}
fn limits() -> Result<Limits> {
    Ok(Limits {
        timeout: Duration::from_secs(WALL),
        max_output_chars: DIAGNOSTICS,
        cpu_seconds: confined_execution::limit_value(libc::RLIMIT_CPU, Some(CPU))?,
        address_space_bytes: confined_execution::limit_value(libc::RLIMIT_AS, Some(MEMORY))?,
    })
}
fn resource(resource: libc::__rlimit_resource_t) -> Result<u64> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: libc writes exactly one initialized resource-limit value.
    ensure!(
        unsafe { libc::getrlimit(resource, &mut current) } == 0,
        std::io::Error::last_os_error()
    );
    ensure!(
        current.rlim_cur == current.rlim_max,
        "worker resource ceiling is not hard"
    );
    Ok(current.rlim_cur)
}
fn bound_current() -> Result<Kernel> {
    let limits = limits()?;
    for (resource, requested) in [
        (
            libc::RLIMIT_CPU,
            limits.cpu_seconds.context("missing CPU ceiling")?,
        ),
        (
            libc::RLIMIT_AS,
            limits
                .address_space_bytes
                .context("missing address-space ceiling")?,
        ),
        (
            libc::RLIMIT_FSIZE,
            confined_execution::limit_value(libc::RLIMIT_FSIZE, Some(FRAME as u64))?
                .context("missing result ceiling")?,
        ),
        (libc::RLIMIT_CORE, 0),
    ] {
        let value = libc::rlimit {
            rlim_cur: requested,
            rlim_max: requested,
        };
        // SAFETY: fixed scalar resources and initialized limits, before Git parsing.
        ensure!(
            unsafe { libc::setrlimit(resource, &value) } == 0,
            std::io::Error::last_os_error()
        );
    }
    // Direct hidden entry also has a kernel wall alarm; no environment-token bypass.
    // Uninterruptible kernel I/O still needs the documented outer watchdog.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    ensure!(
        unsafe { libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) } == 0,
        std::io::Error::last_os_error()
    );
    let mut alarm: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut alarm);
        libc::sigaddset(&mut alarm, libc::SIGALRM);
    }
    let code = unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &alarm, std::ptr::null_mut()) };
    ensure!(code == 0, std::io::Error::from_raw_os_error(code));
    unsafe {
        libc::alarm(WALL as u32);
    }
    let observed = Kernel {
        cpu_seconds: resource(libc::RLIMIT_CPU)?,
        address_space_bytes: resource(libc::RLIMIT_AS)?,
        file_bytes: resource(libc::RLIMIT_FSIZE)?,
        core_bytes: resource(libc::RLIMIT_CORE)?,
    };
    validate_kernel(&observed)?;
    Ok(observed)
}
fn validate_kernel(observed: &Kernel) -> Result<()> {
    ensure!(
        (1..=CPU).contains(&observed.cpu_seconds)
            && (1..=MEMORY).contains(&observed.address_space_bytes)
            && (1..=FRAME as u64).contains(&observed.file_bytes)
            && observed.core_bytes == 0,
        "worker kernel limits differ from required ceilings"
    );
    Ok(())
}
fn anonymous(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    let fd = file.as_raw_fd();
    ensure!(
        fd >= 3
            && metadata.is_file()
            && metadata.nlink() == 0
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.len() == 0,
        "worker output must be an empty owned anonymous file"
    );
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_GET_SEALS) } == 0,
        "worker output must be an unsealed anonymous memfd"
    );
    let mode = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        mode >= 0 && mode & libc::O_ACCMODE == libc::O_RDWR,
        "worker memfd must be read/write"
    );
    Ok(())
}
pub(super) fn execute(args: &VerifyArgs, fd: i32) -> Result<u8> {
    // Install/verify kernel limits even for a directly invoked hidden leaf.
    let limits = bound_current()?;
    ensure!(
        fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
        "invalid private worker descriptor"
    );
    // SAFETY: the internal entry owns its explicit descriptor after validation;
    // no other live File wrapper for it is constructed in this process.
    let mut file = unsafe { File::from_raw_fd(fd) };
    anonymous(&file)?;
    ensure!(
        file.stream_position()? == 0,
        "worker result channel offset is not zero"
    );
    let (verdict, error) = match measure(args) {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(super::unavailable(&format!("{error:#}")).reason)),
    };
    let code = match &verdict {
        Some(value) if value["status"] == "PASS" => 0,
        Some(_) => 1,
        None => 4,
    };
    let bytes = serde_json::to_vec(&Frame {
        schema_version: 1,
        limits,
        verdict,
        error,
    })?;
    ensure!(
        bytes.len() <= FRAME,
        "native receipt worker result exceeds bound"
    );
    file.write_all(&bytes)?;
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) } == 0,
        "seal native receipt worker result: {}",
        std::io::Error::last_os_error()
    );
    Ok(code)
}
fn channel() -> Result<File> {
    // SAFETY: fixed memfd name/flags; a successful descriptor is owned immediately.
    let fd = unsafe {
        libc::memfd_create(
            c"forge-receipt-result".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    ensure!(
        fd >= 0,
        "create receipt result channel: {}",
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(fd) };
    anonymous(&file)?;
    Ok(file)
}
fn command(args: &VerifyArgs, file: &File) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    let mut receipt = OsString::from("--receipt=");
    receipt.push(&args.receipt);
    command
        .args(["receipt", "verify", "--json"])
        .arg(receipt)
        .arg(format!("--tree={}", args.tree))
        .arg(format!("--worker-fd={}", file.as_raw_fd()));
    if let Some(repo) = &args.repo {
        let mut value = OsString::from("--repo=");
        value.push(repo);
        command.arg(value);
    }
    confined_execution::configure_rlimits(&mut command, limits()?);
    let fd = file.as_raw_fd();
    // SAFETY: pre-exec only marks descriptors close-on-exec and uses scalar syscalls.
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}
pub(super) fn supervise(args: &VerifyArgs) -> Result<Value> {
    let mut file = channel()?;
    let interrupts = Interrupts::install()?;
    let output = process_probe::run_interruptible(
        &mut command(args, &file)?,
        Duration::from_secs(WALL),
        DIAGNOSTICS,
        &interrupts,
    )
    .context("native receipt worker supervision failed")?;
    ensure!(
        matches!(output.status.code(), Some(0 | 1 | 4)),
        "native receipt worker failed: {}; {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        output.stdout.is_empty(),
        "native receipt worker emitted unexpected stdout"
    );
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    ensure!(
        seals >= 0 && seals & SEALS == SEALS,
        "native receipt worker result is not sealed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let size = file.metadata()?.len();
    ensure!(
        size > 0 && size <= FRAME as u64,
        "native receipt worker result length exceeds bound"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(FRAME as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == size,
        "native receipt worker result length differs"
    );
    let frame: Frame =
        serde_json::from_slice(&bytes).context("invalid native receipt worker result")?;
    ensure!(
        frame.schema_version == 1 && frame.verdict.is_some() != frame.error.is_some(),
        "invalid native receipt worker result shape"
    );
    validate_kernel(&frame.limits)?;
    ensure!(
        !interrupts.interrupted()?,
        "native receipt worker cancelled before result admission"
    );
    // Preserve the legacy resolution channel: JSON diagnostics to stderr, human to stdout.
    if args.json {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    } else {
        print!("{}", String::from_utf8_lossy(&output.stderr));
    }
    match (frame.verdict, frame.error) {
        (Some(verdict), None) => {
            ensure!(
                verdict["status"] == "PASS" || verdict["status"] == "FAIL",
                "invalid worker verdict status"
            );
            let expected = if verdict["status"] == "PASS" { 0 } else { 1 };
            ensure!(
                output.status.code() == Some(expected),
                "worker status differs from verdict"
            );
            Ok(verdict)
        }
        (None, Some(error)) => {
            ensure!(
                output.status.code() == Some(4),
                "worker status differs from refusal"
            );
            anyhow::bail!("{error}")
        }
        _ => anyhow::bail!("invalid native receipt worker result"),
    }
}
