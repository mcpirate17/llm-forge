//! One owner for process-group teardown. Never reap a leader before killing its group.
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};

pub struct ProcessGroup {
    child: Option<Child>,
    stopped: Option<ExitStatus>,
}

/// A spawned child whose process-group identity has not been validated.
/// Cleanup signals only this still-owned, unreaped child, never its group.
pub struct OwnedChild {
    child: Child,
    stopped: Option<ExitStatus>,
}
pub(crate) struct SpawnFailure {
    pub error: io::Error,
    pub child: Option<OwnedChild>,
}
impl OwnedChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn child(&self) -> &Child {
        &self.child
    }
    pub fn stop(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.stopped {
            return Ok(status);
        }
        if let Err(error) = self.child.kill() {
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        let status = self.child.wait()?;
        self.stopped = Some(status);
        Ok(status)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("forge: ungrouped child cleanup failed: {error}");
        }
    }
}

impl ProcessGroup {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_owned(command).map_err(|failure| {
            let SpawnFailure { error, child } = failure;
            drop(child);
            error
        })
    }
    /// Returns a still-owned child on post-spawn isolation validation failure.
    pub(crate) fn spawn_owned(command: &mut Command) -> Result<Self, SpawnFailure> {
        Self::verify_reaper().map_err(|error| SpawnFailure { error, child: None })?;
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|error| SpawnFailure { error, child: None })?;
        Self::from_child_owned(child)
    }

    pub(crate) fn verify_reaper() -> io::Result<()> {
        // SAFETY: query the disposition into a properly sized initialized buffer.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(io::Error::other(
                "supervisor requires exclusive child reaping and default SIGCHLD disposition",
            ));
        }
        Ok(())
    }

    /// Internal terminal constructor: setsid must have completed before exec.
    pub(crate) fn from_child(child: Child) -> io::Result<Self> {
        Self::from_child_owned(child).map_err(|failure| {
            let SpawnFailure { error, child } = failure;
            drop(child);
            error
        })
    }
    pub(crate) fn from_child_owned(child: Child) -> Result<Self, SpawnFailure> {
        // SAFETY: getpgid observes the live, unreaped child owned here.
        let group = unsafe { libc::getpgid(child.id() as libc::pid_t) };
        if group != child.id() as libc::pid_t {
            let error = if group < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("child must own an isolated process group")
            };
            return Err(SpawnFailure {
                error,
                child: Some(OwnedChild {
                    child,
                    stopped: None,
                }),
            });
        }
        Ok(Self {
            child: Some(child),
            stopped: None,
        })
    }

    /// Nonblocking reap, recording status so Drop never signals a recycled group.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.stopped {
            return Ok(Some(status));
        }
        let status = self.child.as_mut().expect("owned child").try_wait()?;
        if let Some(status) = status {
            self.stopped = Some(status);
        }
        Ok(status)
    }
    /// The owned leader remains unreaped while the process group is signalled.
    pub fn signal(&self, signal: i32) -> io::Result<()> {
        if self.stopped.is_some() {
            return Err(io::Error::other("group leader already reaped"));
        }
        if unsafe { libc::kill(-(self.id() as libc::pid_t), signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub fn child(&self) -> &Child {
        self.child.as_ref().expect("owned child")
    }
    pub fn id(&self) -> u32 {
        self.child().id()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.as_mut().expect("owned child").stdout.take()
    }
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.as_mut().expect("owned child").stdin.take()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.as_mut().expect("owned child").stderr.take()
    }

    /// Transfer process ownership after a background service's readiness check.
    /// The recipient owns termination/reaping; Drop no longer kills this group.
    pub fn into_child(mut self) -> Child {
        self.child.take().expect("owned child")
    }

    pub fn stop(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.stopped {
            return Ok(status);
        }
        // SAFETY: the unreaped child pins its process-group identity. No mutable
        // Child is exposed, so safe callers cannot reap it and create a PID race.
        if unsafe { libc::kill(-(self.id() as libc::pid_t), libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        let status = self.child.as_mut().expect("owned child").wait()?;
        self.stopped = Some(status);
        Ok(status)
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        if let Err(error) = self.stop() {
            eprintln!("forge: process group {} cleanup failed: {error}", self.id());
        }
    }
}

#[cfg(test)]
mod owned_launch_tests {
    use super::*;
    use std::process::Stdio;

    #[test]
    fn invalid_group_retains_live_child_until_explicit_cleanup() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let failure = match ProcessGroup::from_child_owned(child) {
            Ok(_) => panic!("child unexpectedly owned a new process group"),
            Err(failure) => failure,
        };
        let mut child = failure.child.expect("post-spawn child retained");
        assert_eq!(child.id(), pid);
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
        child.stop().unwrap();
        assert_eq!(child.stop().unwrap(), child.stopped.unwrap());
    }

    #[test]
    fn failed_program_spawn_has_no_child() {
        let mut command = Command::new("/definitely-missing-forge-test-executable");
        let failure = match ProcessGroup::spawn_owned(&mut command) {
            Ok(_) => panic!("missing executable unexpectedly ran"),
            Err(failure) => failure,
        };
        assert!(failure.child.is_none());
    }
}
