//! Emulates fchmod on pinned scratch descriptors. Never resumes an intercepted
//! syscall (no CONTINUE), and never reads a pathname pointer from the target.
use anyhow::{ensure, Context, Result};
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

pub(super) struct Broker {
    listener: AsyncFd<OwnedFd>,
    root: Dir,
    path: PathBuf,
}

pub(super) fn check_abi() -> Result<()> {
    // SAFETY: seccomp initializes this fixed ABI query structure.
    let mut sizes: libc::seccomp_notif_sizes = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_GET_NOTIF_SIZES,
            0,
            &mut sizes,
        )
    } != 0
    {
        return Err(io::Error::last_os_error()).context("query seccomp notification ABI");
    }
    ensure!(
        sizes.seccomp_notif as usize == std::mem::size_of::<libc::seccomp_notif>()
            && sizes.seccomp_notif_resp as usize == std::mem::size_of::<libc::seccomp_notif_resp>()
            && sizes.seccomp_data as usize == std::mem::size_of::<libc::seccomp_data>(),
        "unsupported seccomp notification structure sizes"
    );
    Ok(())
}

impl Broker {
    pub(super) fn new(listener: OwnedFd, root: Dir, path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            // SAFETY: the broker transfers its OwnedFd into AsyncFd and only
            // borrows it for ioctls; it never closes or replaces the listener.
            listener: unsafe { AsyncFd::register_with_interest(listener, Interest::READABLE)? },
            root,
            path,
        })
    }

    pub(super) async fn run(self) -> Result<()> {
        let fd = self.listener.get_ref().as_raw_fd();
        loop {
            // NOTIF_RECV has no nonblocking mode. A fresh poll with one consumer
            // guarantees either a notification or EINTR if it was cancelled.
            // Poll before *every* receive; AsyncFd readiness can be cached.
            match ready(fd)? {
                Ready::Closed => return Ok(()),
                Ready::Pending => {
                    self.listener.readable().await?.clear_ready();
                    continue;
                }
                Ready::Request => {}
            }
            let mut request: libc::seccomp_notif = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_RECV, &mut request) } != 0 {
                let error = io::Error::last_os_error();
                if matches!(error.raw_os_error(), Some(libc::EINTR | libc::ENOENT)) {
                    continue;
                }
                return Err(error).context("receive sandbox metadata request");
            }
            let result = self.change_mode(fd, &request);
            let mut response = libc::seccomp_notif_resp {
                id: request.id,
                val: 0,
                flags: 0,
                error: result
                    .err()
                    .map_or(0, |e| -e.raw_os_error().unwrap_or(libc::EIO)),
            };
            if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_SEND, &mut response) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOENT) {
                    return Err(error).context("reply to sandbox metadata request");
                }
            }
            tokio::task::yield_now().await;
        }
    }

    fn change_mode(&self, listener: RawFd, request: &libc::seccomp_notif) -> io::Result<()> {
        let (target, mode) = super::metadata::operation(request)?;
        valid(listener, request.id)?;
        // O_PATH pins the inode without reading data, opening devices or blocking
        // on FIFOs. Validate the notification again after resolving its thread ID.
        let file = super::metadata::open(&self.root, &self.path, request, target)?;
        valid(listener, request.id)?;
        let actual_kind = self.check_file(&file)?;
        let requested_kind = mode & libc::S_IFMT;
        if requested_kind != 0 && requested_kind != actual_kind {
            return Err(denied());
        }
        // The pinned descriptor is the operation's target; changing the child's
        // fd table or pathname cannot redirect this operation after validation.
        if unsafe {
            libc::syscall(
                libc::SYS_fchmodat2,
                file.as_raw_fd(),
                c"".as_ptr(),
                mode & 0o777,
                libc::AT_EMPTY_PATH,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn check_file(&self, file: &File) -> io::Result<u32> {
        let metadata = file.metadata()?;
        if !(metadata.is_file() || metadata.is_dir())
            || (metadata.is_file() && metadata.nlink() != 1)
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(denied());
        }
        let path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        let relative = path.strip_prefix(&self.path).map_err(|_| denied())?;
        if relative == Path::new("") {
            return Err(denied());
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW);
        let inside = self
            .root
            .open_with(relative, &options)?
            .into_std()
            .metadata()?;
        if (inside.dev(), inside.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(denied());
        }
        Ok(metadata.mode() & libc::S_IFMT)
    }
}

fn denied() -> io::Error {
    io::Error::from_raw_os_error(libc::EPERM)
}

fn valid(listener: RawFd, mut id: u64) -> io::Result<()> {
    if unsafe { libc::ioctl(listener, libc::SECCOMP_IOCTL_NOTIF_ID_VALID, &mut id) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

enum Ready {
    Request,
    Pending,
    Closed,
}

fn ready(fd: RawFd) -> io::Result<Ready> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::other("sandbox listener poll failed"));
    }
    if poll.revents & libc::POLLHUP != 0 {
        return Ok(Ready::Closed);
    }
    Ok(if poll.revents & libc::POLLIN != 0 {
        Ready::Request
    } else {
        Ready::Pending
    })
}
