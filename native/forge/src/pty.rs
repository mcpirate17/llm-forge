//! Linux controlling terminals. Full descendant isolation belongs to the launch policy.
use crate::process_group::ProcessGroup;
use crate::process_io::Descriptor;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

pub struct Terminal {
    master: Descriptor,
}

impl Terminal {
    /// Call within the supervisor's reactor, with any cgroup/sandbox wrapper
    /// already selected. The terminal itself is not a confinement boundary.
    pub fn spawn(
        mut command: Command,
        rows: u16,
        columns: u16,
    ) -> io::Result<(Self, ProcessGroup)> {
        dimensions(rows, columns)?;
        ProcessGroup::verify_reaper()?;
        let master = open_master()?;
        // SAFETY: master is a live PTY descriptor, operations take no pointers.
        if unsafe { libc::grantpt(master.as_raw_fd()) } != 0
            || unsafe { libc::unlockpt(master.as_raw_fd()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // TIOCGPTPEER opens the paired slave atomically, without pathname lookup.
        // SAFETY: ioctl returns a new fd; scalar open flags include close-on-exec.
        let raw = unsafe {
            libc::ioctl(
                master.as_raw_fd(),
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful ioctl transferred a unique descriptor.
        let slave = unsafe { OwnedFd::from_raw_fd(raw) };
        let terminal = Self {
            master: Descriptor::new(master)?,
        };
        terminal.resize(rows, columns)?;
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe syscalls, scalar state and no allocation
        // occur after fork. Stdio setup precedes pre_exec, so fd 0 is the slave.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0
                    || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
                Ok(())
            });
        }
        let group = ProcessGroup::from_child(command.spawn()?)?;
        Ok((terminal, group))
    }

    pub async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.master.read(buffer).await {
                // Linux reports EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    pub async fn write(&self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            match self.master.write(buffer).await {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    pub fn resize(&self, rows: u16, columns: u16) -> io::Result<()> {
        dimensions(rows, columns)?;
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: ioctl reads the initialized size through a live borrowed pointer.
        if unsafe { libc::ioctl(self.master.raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Signal the terminal's actual foreground job, including a shell's job-control group.
    pub fn interrupt(&self) -> io::Result<()> {
        // SAFETY: Linux TIOCSIG takes the signal value and validates foreground ownership.
        if unsafe { libc::ioctl(self.master.raw_fd(), libc::TIOCSIG, libc::SIGINT) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn dimensions(rows: u16, columns: u16) -> io::Result<()> {
    if !(1..=1000).contains(&rows) || !(1..=1000).contains(&columns) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal dimensions must be 1..1000",
        ));
    }
    Ok(())
}

fn open_master() -> io::Result<OwnedFd> {
    // SAFETY: posix_openpt returns a new descriptor with atomic close-on-exec.
    let fd = unsafe {
        libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is exclusively owned after successful creation.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
