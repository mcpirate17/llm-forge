//! Process-wide interrupt ownership for a single native supervisor.
//! Install on its main thread before creating worker threads; restore on that thread.
use super::process_io::Descriptor;
use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

static OWNED: AtomicBool = AtomicBool::new(false);

pub struct Interrupts {
    descriptor: OwnedFd,
    previous: libc::sigset_t,
    received: Cell<bool>,
    _same_thread: PhantomData<Rc<()>>,
}

impl AsRawFd for Interrupts {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.descriptor.as_raw_fd()
    }
}

impl Interrupts {
    pub fn install() -> io::Result<Self> {
        if OWNED.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other(
                "another supervisor owns process interrupts",
            ));
        }
        let result = Self::register();
        if result.is_err() {
            OWNED.store(false, Ordering::Release);
        }
        result
    }

    fn register() -> io::Result<Self> {
        // SAFETY: libc initializes valid sigset buffers. Only this thread's mask changes.
        let (mut set, mut previous) = unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGTERM);
        }
        let code = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous) };
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        // SAFETY: signalfd returns a new descriptor for the initialized mask.
        let fd = unsafe { libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            // SAFETY: restore this thread's original initialized mask on failure.
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
            return Err(error);
        }
        Ok(Self {
            // SAFETY: fd is a newly created descriptor exclusively owned here.
            descriptor: unsafe { OwnedFd::from_raw_fd(fd) },
            previous,
            received: Cell::new(false),
            _same_thread: PhantomData,
        })
    }

    pub fn interrupted(&self) -> io::Result<bool> {
        let mut buffer = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
        loop {
            // SAFETY: the descriptor and exclusive destination buffer are valid.
            let count = unsafe {
                libc::read(
                    self.descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count > 0 {
                self.received.set(true);
                continue;
            }
            if count == 0 {
                return Err(io::Error::other("interrupt descriptor closed"));
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => return Ok(self.received.get()),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(error),
            }
        }
    }

    pub fn register_reactor(&self) -> io::Result<Descriptor> {
        Descriptor::new(self.descriptor.try_clone()?)
    }

    /// Child commands must not inherit the supervisor's interrupt-blocking mask.
    pub fn configure_child(&self, command: &mut Command) {
        let previous = self.previous;
        // SAFETY: sigprocmask is async-signal-safe and touches no allocated state.
        unsafe {
            command.pre_exec(move || {
                if libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        if let Err(error) = self.interrupted() {
            eprintln!("forge: drain interrupts: {error}");
        }
        // SAFETY: !Send keeps destruction on the same thread as installation.
        let code = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
        if code != 0 {
            eprintln!(
                "forge: restore interrupt mask: {}",
                io::Error::from_raw_os_error(code)
            );
        }
        OWNED.store(false, Ordering::Release);
    }
}
