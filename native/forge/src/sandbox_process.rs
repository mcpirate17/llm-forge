//! Confined launch ownership: a child and its async metadata broker live together.
use super::broker::Broker;
use super::transport;
use crate::process_group::{OwnedChild, ProcessGroup};
use crate::pty::Terminal;
use anyhow::{Context, Result};
use cap_std::fs::Dir;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use tokio::task::JoinHandle;

pub struct CommandPlan {
    pub(super) command: Command,
    pub(super) receiver: UnixDatagram,
    pub(super) root: Dir,
    pub(super) scratch: PathBuf,
}

pub struct ConfinedProcess {
    group: ProcessGroup,
    broker: Option<JoinHandle<Result<()>>>,
}

/// A launched process retained on an owned launch error. Finish the owned
/// cgroup before calling stop or dropping this value.
pub enum OwnedLaunch {
    UnverifiedChild(OwnedChild),
    Group(ProcessGroup),
}
impl OwnedLaunch {
    pub fn id(&self) -> u32 {
        match self {
            Self::UnverifiedChild(child) => child.id(),
            Self::Group(group) => group.id(),
        }
    }
    pub fn child(&self) -> &Child {
        match self {
            Self::UnverifiedChild(child) => child.child(),
            Self::Group(group) => group.child(),
        }
    }
    pub fn stop(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::UnverifiedChild(child) => child.stop(),
            Self::Group(group) => group.stop(),
        }
    }
}
pub struct LaunchFailure {
    pub error: anyhow::Error,
    pub process: Option<OwnedLaunch>,
}
impl std::fmt::Debug for LaunchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchFailure")
            .field("error", &self.error)
            .field("process_id", &self.process.as_ref().map(OwnedLaunch::id))
            .finish()
    }
}
impl std::fmt::Display for LaunchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}
impl std::error::Error for LaunchFailure {}

impl CommandPlan {
    /// Configure literal argv and standard streams before launch.
    pub fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Requires a Tokio reactor. The payload cannot complete a brokered operation
    /// until its listener has been received and the broker starts servicing it.
    pub fn spawn(self) -> Result<ConfinedProcess> {
        self.spawn_owned().map_err(|failure| {
            let LaunchFailure { error, process } = failure;
            drop(process);
            error
        })
    }

    /// Retains any launched child on error; caller must finish its cgroup first.
    pub fn spawn_owned(mut self) -> std::result::Result<ConfinedProcess, LaunchFailure> {
        tokio::runtime::Handle::try_current()
            .context("sandbox launch requires a reactor")
            .map_err(|error| LaunchFailure {
                error,
                process: None,
            })?;
        let group =
            ProcessGroup::spawn_owned(&mut self.command).map_err(|failure| LaunchFailure {
                error: failure.error.into(),
                process: failure.child.map(OwnedLaunch::UnverifiedChild),
            })?;
        attach_owned(group, self.receiver, self.root, self.scratch)
    }

    pub fn spawn_terminal(self, rows: u16, columns: u16) -> Result<(Terminal, ConfinedProcess)> {
        tokio::runtime::Handle::try_current().context("sandbox launch requires a reactor")?;
        let Self {
            command,
            receiver,
            root,
            scratch,
        } = self;
        let (terminal, group) = Terminal::spawn(command, rows, columns)?;
        Ok((terminal, attach(group, receiver, root, scratch)?))
    }
}

fn attach(
    group: ProcessGroup,
    receiver: UnixDatagram,
    root: Dir,
    scratch: PathBuf,
) -> Result<ConfinedProcess> {
    attach_owned(group, receiver, root, scratch).map_err(|failure| {
        let LaunchFailure { error, process } = failure;
        drop(process);
        error
    })
}
fn attach_owned(
    group: ProcessGroup,
    receiver: UnixDatagram,
    root: Dir,
    scratch: PathBuf,
) -> std::result::Result<ConfinedProcess, LaunchFailure> {
    attach_with(group, || {
        let listener =
            transport::receive(receiver.as_raw_fd()).context("receive sandbox listener")?;
        let broker = Broker::new(listener, root, scratch)?;
        Ok(tokio::spawn(broker.run()))
    })
}
fn attach_with(
    group: ProcessGroup,
    make: impl FnOnce() -> Result<JoinHandle<Result<()>>>,
) -> std::result::Result<ConfinedProcess, LaunchFailure> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make));
    match outcome {
        Ok(Ok(broker)) => Ok(ConfinedProcess {
            group,
            broker: Some(broker),
        }),
        Ok(Err(error)) => Err(LaunchFailure {
            error,
            process: Some(OwnedLaunch::Group(group)),
        }),
        Err(_) => Err(LaunchFailure {
            error: anyhow::anyhow!("sandbox broker setup panicked"),
            process: Some(OwnedLaunch::Group(group)),
        }),
    }
}

impl ConfinedProcess {
    pub fn child(&self) -> &Child {
        self.group.child()
    }
    pub fn id(&self) -> u32 {
        self.group.id()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.group.take_stdout()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.group.take_stderr()
    }

    /// Stop owned cgroups before this call when descendants can use job control.
    /// Aborts and joins the broker, surfacing broker errors rather than hiding them.
    pub async fn stop(&mut self) -> Result<ExitStatus> {
        let status = self.group.stop();
        if let Some(broker) = self.broker.take() {
            if !broker.is_finished() {
                broker.abort();
            }
            match broker.await {
                Ok(result) => result.context("sandbox metadata broker")?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error).context("sandbox metadata broker task"),
            }
        }
        Ok(status?)
    }
}

impl Drop for ConfinedProcess {
    fn drop(&mut self) {
        if let Err(error) = self.group.stop() {
            eprintln!("forge: confined process cleanup failed: {error}");
        }
        if let Some(broker) = self.broker.take() {
            broker.abort();
        }
    }
}

#[cfg(test)]
mod owned_launch_tests {
    use super::*;
    use std::process::Stdio;

    fn group() -> ProcessGroup {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessGroup::spawn(&mut command).unwrap()
    }
    fn assert_retained(mut failure: LaunchFailure, pid: u32) {
        let process = failure.process.as_mut().expect("launched group retained");
        assert_eq!(process.id(), pid);
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0);
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(
            unsafe { info.si_pid() },
            0,
            "child exited before explicit cleanup"
        );
        process.stop().unwrap();
    }
    #[test]
    fn attach_error_and_panic_retain_launched_group() {
        let process = group();
        let pid = process.id();
        let failure = match attach_with(process, || anyhow::bail!("listener failure")) {
            Ok(_) => panic!("expected attach failure"),
            Err(failure) => failure,
        };
        assert!(failure.error.to_string().contains("listener failure"));
        assert_retained(failure, pid);
        let process = group();
        let pid = process.id();
        let failure = match attach_with(process, || panic!("injected broker panic")) {
            Ok(_) => panic!("expected attach failure"),
            Err(failure) => failure,
        };
        assert!(failure.error.to_string().contains("panicked"));
        assert_retained(failure, pid);
    }
    #[test]
    fn missing_reactor_fails_without_launch() {
        let (receiver, _sender) = UnixDatagram::pair().unwrap();
        let root = Dir::open_ambient_dir("/", cap_std::ambient_authority()).unwrap();
        let plan = CommandPlan {
            command: Command::new("/bin/sleep"),
            receiver,
            root,
            scratch: PathBuf::from("/"),
        };
        let failure = match plan.spawn_owned() {
            Ok(_) => panic!("no reactor expected"),
            Err(failure) => failure,
        };
        assert!(failure.process.is_none());
        assert!(failure.error.to_string().contains("reactor"));
    }
}
