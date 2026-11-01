//! Readiness-driven supervision. Only descriptors owned by this invocation are watched.
use anyhow::{ensure, Context, Result};
use forge::process_signals::Interrupts;
use std::ffi::CString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

pub(super) enum Event {
    Changed,
    Exited,
    Interrupted,
    Timer,
}

pub(super) struct Watch(std::fs::File);
impl Watch {
    pub(super) fn new(path: &Path) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: a successful descriptor is immediately exclusively owned.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        ensure!(fd >= 0, "mailbox inotify: {}", io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF;
        // SAFETY: pathname is NUL terminated and borrowed for this call.
        ensure!(
            unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0,
            "mailbox watch: {}",
            io::Error::last_os_error()
        );
        Ok(Self(file))
    }

    pub(super) fn drain(&mut self) -> Result<bool> {
        let mut changed = false;
        let mut buffer = [0u8; 16384];
        // Keep event floods bounded; remaining events keep the descriptor ready.
        for _ in 0..64 {
            match self.0.read(&mut buffer) {
                Ok(0) => anyhow::bail!("mailbox watch closed"),
                Ok(count) => changed |= relevant(&buffer[..count])?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("read mailbox events"),
            }
        }
        Ok(changed)
    }

    pub(super) fn wait(
        &mut self,
        deadline: Instant,
        interrupts: &Interrupts,
        endpoint: Option<&OwnedFd>,
    ) -> Result<Event> {
        loop {
            if interrupts.interrupted()? {
                return Ok(Event::Interrupted);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Event::Timer);
            };
            let mut descriptors = [
                self.0.as_raw_fd(),
                interrupts.as_raw_fd(),
                endpoint.map_or(-1, AsRawFd::as_raw_fd),
            ]
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            let millis = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
            // SAFETY: poll updates the initialized descriptor array.
            if unsafe { libc::poll(descriptors.as_mut_ptr(), 3, millis) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error).context("wait for mailbox readiness");
            }
            ensure!(
                descriptors
                    .iter()
                    .all(|p| p.revents & (libc::POLLNVAL | libc::POLLERR) == 0),
                "mailbox supervision descriptor failed"
            );
            if interrupts.interrupted()? {
                return Ok(Event::Interrupted);
            }
            if descriptors[2].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                return Ok(Event::Exited);
            }
            if descriptors[0].revents != 0 && self.drain()? {
                return Ok(Event::Changed);
            }
        }
    }
}

fn relevant(bytes: &[u8]) -> Result<bool> {
    let mut offset = 0;
    let mut changed = false;
    while offset < bytes.len() {
        ensure!(bytes.len() - offset >= 16, "truncated inotify record");
        let mask = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into()?);
        let length = u32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into()?) as usize;
        let end = offset
            .checked_add(16 + length)
            .context("inotify length overflow")?;
        ensure!(end <= bytes.len(), "truncated inotify filename");
        ensure!(
            mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) == 0,
            "mailbox directory watch invalidated"
        );
        let name = &bytes[offset + 16..end];
        changed |= mask & libc::IN_Q_OVERFLOW != 0 || name.starts_with(b"store.sqlite");
        offset = end;
    }
    Ok(changed)
}

pub(super) fn pidfd(child: &std::process::Child) -> Result<OwnedFd> {
    // SAFETY: this unreaped, owned child pins its PID until group teardown.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::pid_t, 0u32) };
    ensure!(
        fd >= 0,
        "open receiver pidfd: {}",
        io::Error::last_os_error()
    );
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
