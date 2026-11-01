//! One owner for process-group teardown. Never reap a leader before killing its group.
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};

pub struct ProcessGroup {
    child: Option<Child>,
    stopped: Option<ExitStatus>,
}

impl ProcessGroup {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::verify_reaper()?;
        command.process_group(0);
        Self::from_child(command.spawn()?)
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
    pub(crate) fn from_child(mut child: Child) -> io::Result<Self> {
        // SAFETY: getpgid observes the live, unreaped child owned by this function.
        let group = unsafe { libc::getpgid(child.id() as libc::pid_t) };
        if group != child.id() as libc::pid_t {
            let error = if group < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("child must own an isolated process group")
            };
            match child.kill() {
                Ok(()) => {
                    if let Err(cleanup) = child.wait() {
                        eprintln!("forge: invalid group reaping failed: {cleanup}");
                    }
                }
                Err(cleanup) => {
                    eprintln!("forge: invalid group child termination failed: {cleanup}")
                }
            }
            return Err(error);
        }
        Ok(Self {
            child: Some(child),
            stopped: None,
        })
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
