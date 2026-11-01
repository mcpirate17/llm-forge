//! Kernel notifications for queue/worker state changes; no periodic stat loop.
use std::ffi::CString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

pub(super) struct Directory(std::fs::File);
impl Directory {
    pub(super) fn new(path: &Path) -> io::Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: successful descriptors are immediately owned. add_watch only
        // borrows the NUL-terminated pathname during this syscall.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF;
        if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }
    pub(super) fn wait(&mut self, deadline: Instant) -> io::Result<()> {
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            let mut descriptor = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
            // SAFETY: poll receives one initialized descriptor for this call.
            if unsafe { libc::poll(&mut descriptor, 1, millis) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("graph refresh watcher failed"));
            }
            let mut bytes = [0; 4096];
            match self.0.read(&mut bytes) {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}
