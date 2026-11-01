//! Owned nonblocking descriptors on one Tokio reactor; Linux pidfds retain child identity.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::pin::Pin;
use std::process::{Child, ExitStatus};
use std::task::{ready, Context, Poll};
use tokio::io::{unix::AsyncFd, AsyncRead, AsyncWrite, Interest, ReadBuf};

pub struct Descriptor(AsyncFd<OwnedFd>);

/// A CLI-owned standard stream with reversible nonblocking flags. Duplicates
/// share the inherited open-file description, so restore it on normal teardown.
pub struct StandardStream {
    fd: OwnedFd,
    flags: i32,
}
impl StandardStream {
    pub fn new(fd: std::os::fd::RawFd) -> io::Result<Self> {
        // SAFETY: fcntl queries a borrowed descriptor and duplicates it on success.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = Self {
            fd: unsafe { OwnedFd::from_raw_fd(copy) },
            flags,
        };
        make_nonblocking(&stream.fd)?;
        Ok(stream)
    }
    pub fn try_clone(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }
}
impl AsRawFd for StandardStream {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}
impl Drop for StandardStream {
    fn drop(&mut self) {
        // SAFETY: the original open-file description remains pinned by this fd.
        if unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, self.flags) } < 0 {
            eprintln!(
                "forge: could not restore stdio flags: {}",
                io::Error::last_os_error()
            );
        }
    }
}

impl Descriptor {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        make_nonblocking(&fd)?;
        // SAFETY: AsyncFd owns this descriptor. No API replaces or closes it
        // before deregistration; raw_fd only lends its numeric identifier.
        Ok(Self(unsafe { AsyncFd::register(fd)? }))
    }

    /// Cancellation-safe: each successful readiness operation consumes one read.
    pub async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.0
            .async_io(Interest::READABLE, |fd| {
                // SAFETY: buffer is exclusively borrowed and valid for its length.
                result(unsafe {
                    libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len())
                })
            })
            .await
    }

    /// Returns the accepted prefix; callers retain any unwritten suffix on timeout.
    pub async fn write(&self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.0
            .async_io(Interest::WRITABLE, |fd| {
                // SAFETY: buffer is borrowed and valid for its length.
                result(unsafe { libc::write(fd.as_raw_fd(), buffer.as_ptr().cast(), buffer.len()) })
            })
            .await
    }

    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.0.get_ref().as_raw_fd()
    }
}

fn result(count: libc::ssize_t) -> io::Result<usize> {
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

impl AsyncRead for Descriptor {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            let mut readiness = ready!(self.0.poll_read_ready(cx))?;
            match readiness.try_io(|fd| {
                let bytes = buffer.initialize_unfilled();
                // SAFETY: initialized writable destination is borrowed for this syscall.
                result(unsafe {
                    libc::read(
                        fd.get_ref().as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                })
            }) {
                Ok(Ok(count)) => {
                    buffer.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => continue,
            }
        }
    }
}

impl AsyncWrite for Descriptor {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            let mut readiness = ready!(self.0.poll_write_ready(cx))?;
            match readiness.try_io(|fd| {
                // SAFETY: source remains borrowed for this syscall.
                result(unsafe {
                    libc::write(fd.get_ref().as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
                })
            }) {
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct ChildExit(AsyncFd<OwnedFd>);

impl ChildExit {
    pub fn new(child: &Child) -> io::Result<Self> {
        let fd = open_pidfd(child)?;
        // SAFETY: the newly opened pidfd is owned exclusively by AsyncFd and
        // remains unchanged until its registration is dropped.
        Ok(Self(unsafe {
            AsyncFd::register_with_interest(fd, Interest::READABLE)?
        }))
    }

    /// Observe without reaping. The caller kills the process group before wait(),
    /// so an unrelated process can never inherit the leader's recycled identity.
    pub async fn wait(&self) -> io::Result<ExitStatus> {
        let _ready = self.0.readable().await?;
        loop {
            // SAFETY: waitid initializes the valid siginfo buffer for an owned pidfd.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let status = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.0.get_ref().as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if status < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: a successful WEXITED waitid initializes these union members.
            let code = unsafe { info.si_status() };
            let raw = match info.si_code {
                libc::CLD_EXITED if (0..=255).contains(&code) => code << 8,
                libc::CLD_KILLED | libc::CLD_DUMPED if (1..=127).contains(&code) => {
                    code | if info.si_code == libc::CLD_DUMPED {
                        0x80
                    } else {
                        0
                    }
                }
                _ => {
                    return Err(io::Error::other(
                        "pidfd became ready without an exited child",
                    ))
                }
            };
            return Ok(ExitStatus::from_raw(raw));
        }
    }
}

pub(crate) fn make_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: the borrowed descriptor stays live throughout these operations.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn open_pidfd(child: &Child) -> io::Result<OwnedFd> {
    // SAFETY: the unreaped owned child pins the identity; success returns a new fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::pid_t, 0u32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
