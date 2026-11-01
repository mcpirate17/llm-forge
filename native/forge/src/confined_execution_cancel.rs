use anyhow::{Context, Result};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
pub struct Cancellation {
    event: OwnedFd,
    cancelled: AtomicBool,
}
impl Cancellation {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: a successful eventfd syscall returns a newly owned descriptor.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("create execution cancellation event");
        }
        Ok(Self {
            event: unsafe { OwnedFd::from_raw_fd(fd) },
            cancelled: AtomicBool::new(false),
        })
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    /// Level-triggered; no waiter consumes the shared cancellation event.
    pub async fn cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            return Ok(());
        }
        // SAFETY: AsyncFd exclusively owns this cloned descriptor; its identity
        // remains unchanged until reactor deregistration and destruction.
        let event = unsafe { tokio::io::unix::AsyncFd::register(self.event.try_clone()?)? };
        loop {
            if self.is_cancelled() {
                return Ok(());
            }
            let mut ready = event.readable().await.context("wait for cancellation")?;
            if self.is_cancelled() {
                return Ok(());
            }
            ready.clear_ready();
        }
    }
    pub fn cancel(&self) -> Result<()> {
        self.cancelled.store(true, Ordering::Release);
        let value = 1u64.to_ne_bytes();
        loop {
            // SAFETY: write borrows the descriptor and exactly eight bytes.
            let written =
                unsafe { libc::write(self.event.as_raw_fd(), value.as_ptr().cast(), value.len()) };
            if written == 8 {
                return Ok(());
            }
            let error = if written < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::from_raw_os_error(libc::EIO)
            };
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(error).context("wake cancelled execution"),
            }
        }
    }
}
impl AsRawFd for Cancellation {
    fn as_raw_fd(&self) -> RawFd {
        self.event.as_raw_fd()
    }
}
