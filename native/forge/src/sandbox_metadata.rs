//! Copy path operands once, resolve beneath a pinned scratch capability, then
//! operate on the descriptor. No target syscall is ever continued.
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub(super) enum Target {
    Descriptor(u64),
    Path {
        directory: i32,
        pointer: u64,
        empty: bool,
    },
}

pub(super) fn operation(request: &libc::seccomp_notif) -> io::Result<(Target, u32)> {
    let args = request.data.args;
    let (target, mode) = match request.data.nr as i64 {
        libc::SYS_fchmod => (Target::Descriptor(args[0]), args[1]),
        #[cfg(target_arch = "x86_64")]
        libc::SYS_chmod => (
            Target::Path {
                directory: libc::AT_FDCWD,
                pointer: args[0],
                empty: false,
            },
            args[1],
        ),
        libc::SYS_fchmodat => (
            Target::Path {
                directory: args[0] as i32,
                pointer: args[1],
                empty: false,
            },
            args[2],
        ),
        libc::SYS_fchmodat2 if args[3] == 0 || args[3] == libc::AT_EMPTY_PATH as u64 => (
            Target::Path {
                directory: args[0] as i32,
                pointer: args[1],
                empty: args[3] != 0,
            },
            args[2],
        ),
        _ => return Err(denied()),
    };
    // std::fs::copy passes st_mode, including the source file type. Carry only
    // recognized type bits to the broker, which checks the pinned target type.
    let kind = mode & u64::from(libc::S_IFMT);
    if mode & !(0o777 | u64::from(libc::S_IFMT)) != 0 || !matches!(kind, 0 | 0o100000 | 0o040000) {
        return Err(denied());
    }
    Ok((target, mode as u32))
}

pub(super) fn open(
    root: &Dir,
    scratch: &Path,
    request: &libc::seccomp_notif,
    target: Target,
) -> io::Result<File> {
    let path = match target {
        Target::Descriptor(fd) => return descriptor(request.pid, fd),
        Target::Path {
            directory,
            pointer,
            empty,
        } => {
            let path = copied_path(request.pid, pointer)?;
            if path.as_os_str().is_empty() {
                if !empty || directory == libc::AT_FDCWD {
                    return Err(denied());
                }
                return descriptor(request.pid, directory as u64);
            }
            if path.is_absolute() {
                path
            } else {
                let anchor = if directory == libc::AT_FDCWD {
                    format!("/proc/{}/cwd", request.pid)
                } else if directory >= 0 {
                    format!("/proc/{}/fd/{directory}", request.pid)
                } else {
                    return Err(denied());
                };
                let anchor = std::fs::read_link(anchor)?;
                if !anchor.starts_with(scratch) {
                    return Err(denied());
                }
                anchor.join(path)
            }
        }
    };
    let relative = path.strip_prefix(scratch).map_err(|_| denied())?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW);
    Ok(root.open_with(relative, &options)?.into_std())
}

fn descriptor(pid: u32, fd: u64) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    if fd > i32::MAX as u64 {
        return Err(denied());
    }
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(format!("/proc/{pid}/fd/{fd}"))
}

fn copied_path(pid: u32, pointer: u64) -> io::Result<PathBuf> {
    if pointer == 0 || pointer > usize::MAX as u64 {
        return Err(io::Error::from_raw_os_error(libc::EFAULT));
    }
    let mut bytes = [0u8; 4096];
    let mut length = 0usize;
    while length < bytes.len() {
        let address = (pointer as usize).checked_add(length).ok_or_else(denied)?;
        // Read to page boundaries: a short valid string need not have a mapped
        // page after it. Four-KiB splits also work with larger kernel page sizes.
        let count = (4096 - address % 4096).min(bytes.len() - length);
        let local = libc::iovec {
            iov_base: bytes[length..].as_mut_ptr().cast(),
            iov_len: count,
        };
        let remote = libc::iovec {
            iov_base: address as *mut _,
            iov_len: count,
        };
        // SAFETY: kernel copies bounded target memory into a live local buffer.
        let read = unsafe { libc::process_vm_readv(pid as i32, &local, 1, &remote, 1, 0) };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            return Err(io::Error::from_raw_os_error(libc::EFAULT));
        }
        let end = length + read as usize;
        if let Some(nul) = bytes[length..end].iter().position(|b| *b == 0) {
            return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(
                &bytes[..length + nul],
            )));
        }
        length = end;
    }
    Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG))
}

fn denied() -> io::Error {
    io::Error::from_raw_os_error(libc::EPERM)
}
