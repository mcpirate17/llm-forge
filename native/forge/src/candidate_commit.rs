//! Exclusive commit wrapper. Snapshot and Git share the same verified lease.
use crate::{
    candidate_commit_snapshot::{self, Limits},
    candidate_review_lock::ReviewLock,
    candidate_source::Source,
};
use crate::{process_group::ProcessGroup, process_signals::Interrupts};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    ffi::OsString,
    io::IsTerminal,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

#[derive(Args)]
pub struct CommitArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Maximum visited paths, including ignored files and directories.
    #[arg(long, default_value_t = 100_000)]
    max_entries: usize,
    /// Maximum bytes in a single admitted file or symlink.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_file_bytes: u64,
    /// Maximum total bytes in each recovery tree.
    #[arg(long, default_value_t = 2 * 1024 * 1024 * 1024)]
    max_total_bytes: u64,
    /// Arguments passed to `git commit`, after `--`.
    #[arg(last = true)]
    git_args: Vec<OsString>,
}
#[derive(Debug)]
pub struct ChildSignal(pub i32);
impl std::fmt::Display for ChildSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Git terminated by signal {}", self.0)
    }
}
impl std::error::Error for ChildSignal {}
fn validate(args: &CommitArgs) -> Result<Limits> {
    ensure!(
        args.max_entries > 0 && args.max_entries <= 100_000,
        "invalid snapshot entry limit"
    );
    ensure!(
        args.max_file_bytes > 0 && args.max_file_bytes <= 64 * 1024 * 1024,
        "invalid snapshot file byte limit"
    );
    ensure!(
        args.max_total_bytes > 0 && args.max_total_bytes <= 2 * 1024 * 1024 * 1024,
        "invalid snapshot total byte limit"
    );
    for name in ["GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"] {
        ensure!(
            std::env::var_os(name).is_none(),
            "{name} is unsupported for candidate commit; select the repository with --repo"
        );
    }
    Ok(Limits {
        max_entries: args.max_entries,
        max_file_bytes: args.max_file_bytes,
        max_total_bytes: args.max_total_bytes,
    })
}
fn check_git_environment(source: &Source) -> Result<()> {
    for (name, expected) in [
        ("GIT_DIR", source.repository().path()),
        ("GIT_WORK_TREE", source.root()),
        ("GIT_COMMON_DIR", source.common_dir()),
    ] {
        if let Some(value) = std::env::var_os(name) {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() {
                path
            } else {
                source.root().join(path)
            };
            ensure!(
                path.canonicalize()
                    .with_context(|| format!("resolve {name}"))?
                    == expected.canonicalize()?,
                "{name} does not match the selected repository"
            );
        }
    }
    Ok(())
}
// Background launchers can inherit SIG_IGN; ignored signals never reach signalfd.
// The CLI owns these dispositions while its single-threaded supervisor runs.
struct InterruptDispositions {
    int: libc::sigaction,
    term: libc::sigaction,
}
impl InterruptDispositions {
    fn install() -> Result<Self> {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut action.sa_mask);
            let mut int = std::mem::zeroed();
            let mut term = std::mem::zeroed();
            if libc::sigaction(libc::SIGINT, &action, &mut int) != 0 {
                return Err(std::io::Error::last_os_error()).context("configure SIGINT");
            }
            if libc::sigaction(libc::SIGTERM, &action, &mut term) != 0 {
                let error = std::io::Error::last_os_error();
                libc::sigaction(libc::SIGINT, &int, std::ptr::null_mut());
                return Err(error).context("configure SIGTERM");
            }
            Ok(Self { int, term })
        }
    }
}
impl Drop for InterruptDispositions {
    fn drop(&mut self) {
        unsafe {
            if libc::sigaction(libc::SIGINT, &self.int, std::ptr::null_mut()) != 0 {
                eprintln!("forge: restore SIGINT: {}", std::io::Error::last_os_error());
            }
            if libc::sigaction(libc::SIGTERM, &self.term, std::ptr::null_mut()) != 0 {
                eprintln!(
                    "forge: restore SIGTERM: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}
fn parent_death_signal(command: &mut Command) {
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other("commit supervisor died before exec"));
            }
            Ok(())
        });
    }
}
fn reap_group(mut group: ProcessGroup, signal: i32) -> Result<ExitStatus> {
    if let Err(error) = group.signal(signal) {
        drop(group.into_child());
        return Err(error).context("forward interrupt to Git group");
    }
    // The unreaped leader pins the process-group identity through the final kill.
    std::thread::sleep(Duration::from_secs(1));
    if let Err(error) = group.signal(libc::SIGKILL) {
        drop(group.into_child());
        return Err(error).context("kill interrupted Git group");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match group.try_wait() {
            Ok(Some(_)) => return Err(ChildSignal(signal).into()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                drop(group.into_child());
                anyhow::bail!("interrupted Git did not reap within 5 seconds; inherited descriptors retain the lease");
            }
            Err(error) => {
                drop(group.into_child());
                return Err(error).context("reap interrupted Git");
            }
        }
    }
}
fn wait_group(command: &mut Command, interrupts: &Interrupts) -> Result<ExitStatus> {
    let mut group = ProcessGroup::spawn(command).context("spawn isolated Git")?;
    loop {
        match interrupts.signal() {
            Ok(Some(signal)) => return reap_group(group, signal),
            Ok(None) => (),
            Err(error) => {
                if let Err(kill_error) = group.signal(libc::SIGKILL) {
                    eprintln!("forge: kill Git group after supervisor error: {kill_error}");
                }
                drop(group.into_child());
                return Err(error).context("read Git interrupt signal");
            }
        }
        match group.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => (),
            Err(error) => {
                // A failed wait may mean the leader was reaped elsewhere; do not
                // signal a group whose numeric identity could now be recycled.
                drop(group.into_child());
                return Err(error).context("wait for Git");
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
struct InteractiveChild {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for InteractiveChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Err(error) = self.child.kill() {
            if error.raw_os_error() != Some(libc::ESRCH) {
                eprintln!("forge: kill interactive Git: {error}");
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    eprintln!("forge: interactive Git did not reap; inherited descriptors retain the lease");
                    return;
                }
                Err(error) => {
                    eprintln!("forge: reap interactive Git: {error}");
                    return;
                }
            }
        }
    }
}
fn wait_interactive(command: &mut Command, interrupts: &Interrupts) -> Result<ExitStatus> {
    let mut owned = InteractiveChild {
        child: command.spawn().context("spawn interactive Git")?,
        reaped: false,
    };
    loop {
        if let Some(signal) = interrupts.signal()? {
            eprintln!("forge candidate commit: interrupting Git; interactive hook descendants may continue and retain the lease");
            let pid = owned.child.id() as libc::pid_t;
            if unsafe { libc::kill(pid, signal) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
            std::thread::sleep(Duration::from_secs(1));
            if let Err(error) = owned.child.kill() {
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if owned.child.try_wait()?.is_some() {
                    owned.reaped = true;
                    return Err(ChildSignal(signal).into());
                }
                if Instant::now() >= deadline {
                    owned.reaped = true;
                    anyhow::bail!("interrupted interactive Git did not reap within 5 seconds; inherited descriptors retain the lease");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if let Some(status) = owned.child.try_wait()? {
            owned.reaped = true;
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn run_git(command: &mut Command, interrupts: &Interrupts) -> Result<ExitStatus> {
    interrupts.configure_child(command);
    parent_death_signal(command);
    if std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::io::stderr().is_terminal()
    {
        wait_interactive(command, interrupts)
    } else {
        wait_group(command, interrupts)
    }
}
pub fn run(args: CommitArgs) -> Result<u8> {
    let limits = validate(&args)?;
    let source = Source::open(&args.repo)?;
    check_git_environment(&source)?;
    let _dispositions = InterruptDispositions::install()?;
    let interrupts = Interrupts::install().context("install commit interrupt handler")?;
    if let Some(signal) = interrupts.signal()? {
        return Err(ChildSignal(signal).into());
    }
    let lease = ReviewLock::acquire_exclusive_cancellable(
        source.root(),
        Duration::from_secs(60),
        &mut || {
            if let Some(signal) = interrupts.signal()? {
                return Err(ChildSignal(signal).into());
            }
            Ok(())
        },
    )?;
    if let Some(signal) = interrupts.signal()? {
        return Err(ChildSignal(signal).into());
    }
    let recovery = candidate_commit_snapshot::create(&source, limits)
        .context("create pre-commit recovery snapshot")?;
    eprintln!(
        "forge candidate commit: recovery snapshot {} (staged ^1 tree {}, worktree ^2 tree {})",
        recovery.reference, recovery.staged_tree, recovery.worktree_tree
    );
    if let Some(signal) = interrupts.signal()? {
        return Err(ChildSignal(signal).into());
    }
    let mut command = Command::new("git");
    command
        .current_dir(source.root())
        .arg("commit")
        .args(&args.git_args);
    lease.prepare_child(&mut command)?;
    let status = run_git(&mut command, &interrupts)?;
    lease.verify()?;
    drop(lease);
    if let Some(code) = status.code() {
        return u8::try_from(code).context("invalid Git exit code");
    }
    if let Some(signal) = status.signal() {
        return Err(ChildSignal(signal).into());
    }
    anyhow::bail!("Git returned neither an exit code nor a signal")
}
