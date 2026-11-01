//! Small native seccomp filter. Landlock supplies path and process-domain checks;
//! this layer closes network, metadata-mutation, alternate-ABI and namespace gaps.
use anyhow::{bail, Result};
use std::io;

const LOAD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const EQUAL: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
const BITS: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const RETURN: u16 = (libc::BPF_RET | libc::BPF_K) as u16;
const DENY: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
// Linux asm-generic/fcntl.h; these are not exported by libc on every target.
const F_SETOWN_EX: i32 = 15;
const F_SETSIG: i32 = 10;

fn op(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

pub(super) fn instructions() -> Result<Box<[libc::sock_filter]>> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => 0xc000003e,
        "aarch64" => 0xc00000b7,
        other => bail!("sandbox seccomp architecture is unsupported: {other}"),
    };
    let mut code = vec![
        op(LOAD, 0, 0, 4),
        op(EQUAL, 1, 0, arch),
        op(RETURN, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        op(LOAD, 0, 0, 0),
    ];
    // x32 uses the x86_64 audit architecture but a distinct syscall table.
    code.extend([
        op(BITS, 0, 1, 0x40000000),
        op(RETURN, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
    ]);
    code.extend([
        op(EQUAL, 0, 1, libc::SYS_fchmod as u32),
        op(RETURN, 0, 0, libc::SECCOMP_RET_USER_NOTIF),
    ]);
    for syscall in [libc::SYS_fchmodat, libc::SYS_fchmodat2] {
        code.extend([
            op(EQUAL, 0, 1, syscall as u32),
            op(RETURN, 0, 0, libc::SECCOMP_RET_USER_NOTIF),
        ]);
    }
    #[cfg(target_arch = "x86_64")]
    code.extend([
        op(EQUAL, 0, 1, libc::SYS_chmod as u32),
        op(RETURN, 0, 0, libc::SECCOMP_RET_USER_NOTIF),
    ]);
    // Last explicitly reviewed generic syscall is fchmodat2 (452). New kernel
    // APIs (including setxattrat/fileattr_set) must not silently widen this policy.
    code.extend([
        op(
            (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16,
            0,
            1,
            453,
        ),
        op(RETURN, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
    ]);
    for number in denied() {
        code.extend([op(EQUAL, 0, 1, number as u32), op(RETURN, 0, 0, DENY)]);
    }
    // clone3 has a pointer to flags; ENOSYS requests libc's inspectable clone fallback.
    code.extend([
        op(EQUAL, 0, 1, libc::SYS_clone3 as u32),
        op(RETURN, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
    ]);
    argument_guards(&mut code);
    code.push(op(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW));
    Ok(code.into_boxed_slice())
}

fn argument_guards(code: &mut Vec<libc::sock_filter>) {
    let namespaces = libc::CLONE_NEWCGROUP
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWUSER
        | libc::CLONE_NEWUTS;
    argument_guard(code, libc::SYS_clone, 0, BITS, namespaces as u32, false);
    argument_guard(
        code,
        libc::SYS_socketpair,
        0,
        EQUAL,
        libc::AF_UNIX as u32,
        true,
    );
    ioctl_guard(code);
    for command in [
        libc::F_SETOWN,
        F_SETOWN_EX,
        F_SETSIG,
        libc::F_SETLEASE,
        libc::F_NOTIFY,
    ] {
        argument_guard(code, libc::SYS_fcntl, 1, EQUAL, command as u32, false);
    }
    // Landlock's signal/ptrace scopes do not govern resource changes to other
    // same-UID processes. Only the current-thread/process form is accepted.
    for syscall in [
        libc::SYS_sched_setaffinity,
        libc::SYS_sched_setscheduler,
        libc::SYS_sched_setparam,
        libc::SYS_prlimit64,
    ] {
        argument_guard(code, syscall, 0, EQUAL, 0, true);
    }
    // A newer USER_NOTIF filter could route fchmod to an attacker-owned listener.
    argument_guard(
        code,
        libc::SYS_prctl,
        0,
        EQUAL,
        libc::PR_SET_SECCOMP as u32,
        false,
    );
}

fn ioctl_guard(code: &mut Vec<libc::sock_filter>) {
    // Terminal state only. Filesystem flag-changing ioctls otherwise evade
    // read-only Landlock paths because IoctlDev covers device nodes, not files.
    let allowed = [
        libc::TCGETS,
        libc::TCSETS,
        libc::TCSETSW,
        libc::TCSETSF,
        libc::TCGETA,
        libc::TCSETA,
        libc::TCSETAW,
        libc::TCSETAF,
        libc::TCSBRK,
        libc::TCXONC,
        libc::TCFLSH,
        libc::TIOCSCTTY,
        libc::TIOCNOTTY,
        libc::TIOCGPGRP,
        libc::TIOCSPGRP,
        libc::TIOCGSID,
        libc::TIOCGWINSZ,
        libc::TIOCSWINSZ,
        libc::FIONREAD,
        libc::FIONBIO,
        libc::TIOCOUTQ,
        libc::FIOCLEX,
        libc::FIONCLEX,
    ];
    code.push(op(
        EQUAL,
        0,
        (allowed.len() * 2 + 2) as u8,
        libc::SYS_ioctl as u32,
    ));
    code.push(op(LOAD, 0, 0, 24));
    for request in allowed {
        code.extend([
            op(EQUAL, 0, 1, request as u32),
            op(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW),
        ]);
    }
    code.push(op(RETURN, 0, 0, DENY));
}

fn argument_guard(
    code: &mut Vec<libc::sock_filter>,
    syscall: i64,
    argument: u32,
    comparison: u16,
    value: u32,
    allow_match: bool,
) {
    code.extend([
        op(EQUAL, 0, 4, syscall as u32),
        op(LOAD, 0, 0, 16 + argument * 8),
        op(
            comparison,
            u8::from(allow_match),
            u8::from(!allow_match),
            value,
        ),
        op(RETURN, 0, 0, DENY),
        op(LOAD, 0, 0, 0),
    ]);
}

fn denied() -> Vec<i64> {
    let mut calls = vec![
        libc::SYS_socket,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_move_mount,
        libc::SYS_open_tree,
        libc::SYS_mount_setattr,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_pidfd_getfd,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_acct,
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        libc::SYS_quotactl,
        libc::SYS_seccomp,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_utimensat,
        libc::SYS_setxattr,
        libc::SYS_lsetxattr,
        libc::SYS_fsetxattr,
        libc::SYS_removexattr,
        libc::SYS_lremovexattr,
        libc::SYS_fremovexattr,
        libc::SYS_shmget,
        libc::SYS_shmat,
        libc::SYS_shmctl,
        libc::SYS_shmdt,
        libc::SYS_semget,
        libc::SYS_semctl,
        libc::SYS_semop,
        libc::SYS_semtimedop,
        libc::SYS_msgget,
        libc::SYS_msgsnd,
        libc::SYS_msgrcv,
        libc::SYS_msgctl,
        libc::SYS_mq_open,
        libc::SYS_mq_unlink,
        libc::SYS_setpriority,
        libc::SYS_ioprio_set,
        libc::SYS_sched_setattr,
        libc::SYS_fanotify_init,
    ];
    #[cfg(target_arch = "x86_64")]
    calls.extend([
        libc::SYS_chown,
        libc::SYS_lchown,
        libc::SYS_utime,
        libc::SYS_utimes,
        libc::SYS_futimesat,
        libc::SYS_modify_ldt,
        libc::SYS_iopl,
        libc::SYS_ioperm,
        libc::SYS_uselib,
        libc::SYS_vhangup,
    ]);
    calls
}

pub(super) fn install(instructions: &[libc::sock_filter]) -> io::Result<i32> {
    let program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_ptr().cast_mut(),
    };
    // SAFETY: kernel reads the initialized filter and copies it before return.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &program,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd as i32)
}
