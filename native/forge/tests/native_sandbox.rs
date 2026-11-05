#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::process_io::{ChildExit, Descriptor};
use forge::sandbox::{CommandPlan, Policy, Sandbox};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn policy(host: &support::Host) -> Policy {
    let scratch = host.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("code"), "original").unwrap();
    fs::write(host.0.join("secret"), "confidential").unwrap();
    Policy {
        read_only: vec![
            PathBuf::from("/usr"),
            PathBuf::from("/lib"),
            PathBuf::from("/lib64"),
            PathBuf::from("/etc/ld.so.cache"),
            source,
            std::env::current_exe().unwrap(),
        ],
        scratch: scratch.clone(),
        working_directory: scratch,
        environment: BTreeMap::from([
            ("FORGE_SANDBOX_ROOT".into(), host.0.display().to_string()),
            (
                "FORGE_SANDBOX_PARENT".into(),
                std::process::id().to_string(),
            ),
        ]),
    }
}

fn probe(sandbox: &Sandbox, mode: &str) -> CommandPlan {
    let mut command = sandbox.command(&std::env::current_exe().unwrap()).unwrap();
    command
        .command()
        .args(["--exact", "confined_child", "--nocapture"])
        .env("FORGE_SANDBOX_MODE", mode)
        .stdin(Stdio::null());
    command
}

async fn bytes(descriptor: Descriptor) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = descriptor.read(&mut buffer).await.unwrap();
        if count == 0 {
            return result;
        }
        result.extend_from_slice(&buffer[..count]);
        assert!(result.len() <= 131072);
    }
}

async fn output(mut plan: CommandPlan) -> Output {
    plan.command().stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut process = plan.spawn().unwrap();
    let stdout = Descriptor::new(process.take_stdout().unwrap().into()).unwrap();
    let stderr = Descriptor::new(process.take_stderr().unwrap().into()).unwrap();
    let exited = ChildExit::new(process.child()).unwrap();
    let (status, stdout, stderr) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(exited.wait(), bytes(stdout), bytes(stderr))
    })
    .await
    .unwrap();
    process.stop().await.unwrap();
    Output {
        status: status.unwrap(),
        stdout,
        stderr,
    }
}

async fn success(plan: CommandPlan) {
    let result = output(plan).await;
    assert!(
        result.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn confinement_is_enforced_by_the_kernel_and_inherited_by_children() {
    let host = support::Host::new();
    let sandbox = Sandbox::prepare(policy(&host)).unwrap();
    eprintln!("{}", serde_json::to_string(sandbox.report()).unwrap());
    let before = fs::metadata(host.0.join("source/code")).unwrap();
    let listener = UnixListener::bind(host.0.join("control.sock")).unwrap();
    for mode in [
        "filesystem",
        "network",
        "signals",
        "syscalls",
        "threads",
        "descendant",
        "metadata",
        "descriptor-race",
    ] {
        success(probe(&sandbox, mode)).await;
    }
    assert_eq!(
        fs::read_to_string(host.0.join("source/code")).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(host.0.join("secret")).unwrap(),
        "confidential"
    );
    let after = fs::metadata(host.0.join("source/code")).unwrap();
    assert_eq!(
        (before.mode(), before.uid(), before.gid()),
        (after.mode(), after.uid(), after.gid())
    );
    drop(listener);
}

#[tokio::test(flavor = "current_thread")]
async fn untrusted_preexisting_descriptors_are_closed_and_abis_cannot_bypass_the_filter() {
    let host = support::Host::new();
    let sandbox = Sandbox::prepare(policy(&host)).unwrap();
    let secret = File::open(host.0.join("secret")).unwrap();
    // Deliberately remove CLOEXEC to exercise the spawn boundary, not std defaults.
    assert_eq!(
        unsafe { libc::fcntl(secret.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    let mut plan = probe(&sandbox, "descriptor");
    plan.command()
        .env("FORGE_SANDBOX_FD", secret.as_raw_fd().to_string());
    success(plan).await;
    #[cfg(target_arch = "x86_64")]
    for mode in ["x32", "compat"] {
        let result = output(probe(&sandbox, mode)).await;
        assert_eq!(
            result.status.signal(),
            Some(libc::SIGSYS),
            "{mode}: {result:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn capabilities_are_inode_bound_and_invalid_policy_fails_before_launch() {
    let host = support::Host::new();
    let mut p = policy(&host);
    p.read_only.push(host.0.join("missing"));
    assert!(Sandbox::prepare(p).is_err());
    let host = support::Host::new();
    let mut p = policy(&host);
    p.environment.insert("HOME".into(), "/".into());
    assert!(Sandbox::prepare(p).is_err());
    let host = support::Host::new();
    let p = policy(&host);
    fs::hard_link(host.0.join("secret"), p.scratch.join("alias")).unwrap();
    assert!(Sandbox::prepare(p).is_err());
    let host = support::Host::new();
    let sandbox = Sandbox::prepare(policy(&host)).unwrap();
    fs::rename(host.0.join("source"), host.0.join("moved")).unwrap();
    fs::create_dir(host.0.join("source")).unwrap();
    fs::write(host.0.join("source/code"), "replacement").unwrap();
    success(probe(&sandbox, "inode")).await;
    assert!(sandbox.command(Path::new("relative-program")).is_err());
    success(sandbox.command(Path::new("/bin/true")).unwrap()).await;
}

#[tokio::test(flavor = "current_thread")]
async fn c_compiler_produces_and_runs_an_executable_in_confined_scratch() {
    let host = support::Host::new();
    let sandbox = Sandbox::prepare(policy(&host)).unwrap();
    fs::write(
        host.0.join("scratch/hello.c"),
        "int main(void) { return 0; }\n",
    )
    .unwrap();
    let mut plan = sandbox.command(Path::new("/usr/bin/cc")).unwrap();
    plan.command().args(["hello.c", "-o", "hello"]);
    success(plan).await;
    assert_ne!(
        fs::metadata(host.0.join("scratch/hello")).unwrap().mode() & 0o111,
        0
    );
    success(sandbox.command(&host.0.join("scratch/hello")).unwrap()).await;
}

#[tokio::test(flavor = "current_thread")]
async fn stdlib_copy_and_matching_file_modes_remain_confined() {
    let host = support::Host::new();
    let sandbox = Sandbox::prepare(policy(&host)).unwrap();
    success(probe(&sandbox, "copy-modes")).await;
    assert_eq!(
        fs::read(host.0.join("scratch/copied")).unwrap(),
        b"original"
    );
    assert_eq!(fs::read(host.0.join("source/code")).unwrap(), b"original");
}

#[test]
fn confined_child() {
    let Ok(mode) = std::env::var("FORGE_SANDBOX_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("FORGE_SANDBOX_ROOT").unwrap());
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        1
    );
    assert_eq!(unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) }, 2);
    assert!(std::env::var_os("SSH_AUTH_SOCK").is_none());
    assert_eq!(std::env::var_os("HOME").unwrap(), root.join("scratch"));
    match mode.as_str() {
        "copy-modes" => copied_modes(&root),
        "filesystem" => filesystem(&root),
        "network" => network(&root),
        "signals" => signals(),
        "syscalls" => syscalls(),
        "metadata" => metadata(&root),
        "descriptor-race" => descriptor_race(&root),
        "threads" => {
            let mut threads = Vec::new();
            for _ in 0..16 {
                let path = root.join("secret");
                threads.push(std::thread::spawn(move || assert!(fs::read(path).is_err())));
            }
            for thread in threads {
                thread.join().unwrap();
            }
        }
        "descendant" => {
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "confined_child", "--nocapture"])
                .env("FORGE_SANDBOX_MODE", "network")
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
        }
        "descriptor" => {
            let fd: i32 = std::env::var("FORGE_SANDBOX_FD").unwrap().parse().unwrap();
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
        "inode" => {
            assert!(fs::read(root.join("source/code")).is_err());
            assert_eq!(
                fs::read_to_string(root.join("moved/code")).unwrap(),
                "original"
            );
        }
        #[cfg(target_arch = "x86_64")]
        "x32" => {
            unsafe {
                libc::syscall(0x40000000i64 | libc::SYS_getpid);
            }
            panic!("x32 syscall was not killed");
        }
        #[cfg(target_arch = "x86_64")]
        "compat" => {
            unsafe {
                core::arch::asm!("int 0x80", inout("eax") 20u32 => _);
            }
            panic!("compat syscall was not killed");
        }
        _ => panic!("unknown child probe {mode}"),
    }
}

fn filesystem(root: &Path) {
    let source = root.join("source/code");
    let scratch = root.join("scratch");
    assert_eq!(fs::read_to_string(&source).unwrap(), "original");
    assert!(fs::read(root.join("secret")).is_err());
    assert!(fs::read_dir(root).is_err());
    assert!(fs::write(&source, "damage").is_err());
    assert!(OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&source)
        .is_err());
    assert!(fs::remove_file(&source).is_err());
    assert!(fs::rename(&source, scratch.join("stolen")).is_err());
    assert!(fs::hard_link(&source, scratch.join("hardlink")).is_err());
    assert!(fs::set_permissions(&source, fs::Permissions::from_mode(0o777)).is_err());
    let fd = File::open(&source).unwrap();
    assert_eq!(unsafe { libc::fchmod(fd.as_raw_fd(), 0o777) }, -1);
    symlink(root.join("secret"), scratch.join("escape")).unwrap();
    assert!(fs::read(scratch.join("escape")).is_err());
    assert!(fs::write(scratch.join("escape"), "damage").is_err());
    fs::write(scratch.join("allowed"), "usable").unwrap();
    assert_eq!(
        fs::read_to_string(scratch.join("allowed")).unwrap(),
        "usable"
    );
    fs::rename(scratch.join("allowed"), scratch.join("renamed")).unwrap();
    fs::remove_file(scratch.join("renamed")).unwrap();
    assert!(fs::read(format!(
        "/proc/{}/environ",
        std::env::var("FORGE_SANDBOX_PARENT").unwrap()
    ))
    .is_err());
}

fn network(root: &Path) {
    for (family, kind) in [
        (libc::AF_INET, libc::SOCK_STREAM),
        (libc::AF_INET, libc::SOCK_DGRAM),
        (libc::AF_INET6, libc::SOCK_STREAM),
        (libc::AF_UNIX, libc::SOCK_STREAM),
    ] {
        assert_eq!(unsafe { libc::socket(family, kind, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    assert!(UnixStream::connect(root.join("control.sock")).is_err());
    let (mut first, mut second) = UnixStream::pair().unwrap();
    first.write_all(b"ok").unwrap();
    let mut received = [0; 2];
    second.read_exact(&mut received).unwrap();
    assert_eq!(&received, b"ok");
}

fn signals() {
    let parent: i32 = std::env::var("FORGE_SANDBOX_PARENT")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(parent, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(unsafe { libc::kill(libc::getpid(), 0) }, 0);
    // An external-domain tracer cannot be reached even with an otherwise readable path.
    assert_eq!(
        unsafe { libc::ptrace(libc::PTRACE_SEIZE, parent, 0, 0) },
        -1
    );
}

fn syscalls() {
    for syscall in [
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_bpf,
        libc::SYS_io_uring_setup,
        libc::SYS_pidfd_getfd,
        libc::SYS_keyctl,
        libc::SYS_shmget,
        libc::SYS_msgget,
        libc::SYS_semget,
        libc::SYS_seccomp,
        libc::SYS_setpriority,
        libc::SYS_ioprio_set,
    ] {
        assert_eq!(
            unsafe { libc::syscall(syscall, 0, 0, 0, 0, 0, 0) },
            -1,
            "{syscall}"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM),
            "{syscall}"
        );
    }
    assert_eq!(unsafe { libc::syscall(libc::SYS_clone3, 0, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENOSYS)
    );
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_SECCOMP, 0, 0, 0, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
    for request in [libc::TIOCSTI, libc::TIOCLINUX] {
        assert_eq!(
            unsafe { libc::ioctl(0, request, std::ptr::null::<u8>()) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    for syscall in [453, 463, 466, 10000] {
        assert_eq!(unsafe { libc::syscall(syscall, 0, 0, 0, 0, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS)
        );
    }
    for syscall in [
        libc::SYS_sched_setaffinity,
        libc::SYS_sched_setscheduler,
        libc::SYS_sched_setparam,
        libc::SYS_prlimit64,
    ] {
        assert_eq!(unsafe { libc::syscall(syscall, 1, 0, 0, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    for command in [
        libc::F_SETOWN,
        15, // Linux F_SETOWN_EX
        10, // Linux F_SETSIG
        libc::F_SETLEASE,
        libc::F_NOTIFY,
    ] {
        assert_eq!(unsafe { libc::fcntl(0, command, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    // Anonymous memfd remains usable; no external descriptors are introduced.
    let fd = unsafe { libc::memfd_create(c"test".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0);
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(b"private").unwrap();
}

fn copied_modes(root: &Path) {
    let copied = root.join("scratch/copied");
    fs::copy(root.join("source/code"), &copied).unwrap();
    let file = File::open(&copied).unwrap();
    assert_eq!(
        unsafe { libc::fchmod(file.as_raw_fd(), libc::S_IFREG | 0o640) },
        0
    );
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o640);
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_fchmod,
                file.as_raw_fd(),
                (1u64 << 32) | u64::from(libc::S_IFREG | 0o600),
            )
        },
        -1
    );
    for mode in [
        libc::S_IFDIR | 0o700,
        libc::S_IFIFO | 0o600,
        libc::S_IFLNK | 0o777,
        libc::S_IFREG | 0o4755,
        libc::S_IFREG | 0o2755,
        libc::S_IFREG | 0o1755,
        0x8000_0000 | 0o600,
    ] {
        assert_eq!(
            unsafe { libc::fchmod(file.as_raw_fd(), mode) },
            -1,
            "accepted mode {mode:o}"
        );
    }
    let directory = root.join("scratch/copied-dir");
    fs::create_dir(&directory).unwrap();
    let directory = File::open(directory).unwrap();
    assert_eq!(
        unsafe { libc::fchmod(directory.as_raw_fd(), libc::S_IFDIR | 0o750) },
        0
    );
    assert_eq!(directory.metadata().unwrap().mode() & 0o777, 0o750);
    assert_eq!(
        unsafe { libc::fchmod(directory.as_raw_fd(), libc::S_IFREG | 0o750) },
        -1
    );
}

fn metadata(root: &Path) {
    let scratch = root.join("scratch");
    let path = scratch.join("permissions");
    fs::write(&path, "owned").unwrap();
    let file = File::open(&path).unwrap();
    assert_eq!(unsafe { libc::fchmod(file.as_raw_fd(), 0o750) }, 0);
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o750);
    assert_eq!(unsafe { libc::fchmod(file.as_raw_fd(), 0o4755) }, -1);
    assert_eq!(unsafe { libc::fchmod(1, 0o777) }, -1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let directory = File::open(&scratch).unwrap();
    assert_eq!(
        unsafe { libc::fchmodat(directory.as_raw_fd(), c"permissions".as_ptr(), 0o700, 0) },
        0
    );
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_fchmodat2,
                file.as_raw_fd(),
                c"".as_ptr(),
                0o600,
                libc::AT_EMPTY_PATH,
            )
        },
        0
    );
    // Widening the private scratch boundary itself is always forbidden.
    assert_eq!(unsafe { libc::fchmod(directory.as_raw_fd(), 0o777) }, -1);
    assert!(fs::set_permissions(&scratch, fs::Permissions::from_mode(0o777)).is_err());
    fs::create_dir(scratch.join("subdir")).unwrap();
    fs::set_permissions(scratch.join("subdir"), fs::Permissions::from_mode(0o750)).unwrap();
    symlink(root.join("source/code"), scratch.join("mode-escape")).unwrap();
    assert!(fs::set_permissions(
        scratch.join("mode-escape"),
        fs::Permissions::from_mode(0o777)
    )
    .is_err());
    assert!(fs::set_permissions(
        scratch.join("../source/code"),
        fs::Permissions::from_mode(0o777)
    )
    .is_err());
    fs::hard_link(&path, scratch.join("permissions-alias")).unwrap();
    assert_eq!(unsafe { libc::fchmod(file.as_raw_fd(), 0o777) }, -1);
    fs::remove_file(scratch.join("permissions-alias")).unwrap();
    // Kernel-copied paths end at an inaccessible page; no over-read is required.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page * 2,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let pointer = unsafe { mapping.cast::<u8>().add(page - 12) };
    unsafe {
        std::ptr::copy_nonoverlapping(c"permissions".as_ptr().cast::<u8>(), pointer, 12);
    }
    assert_eq!(
        unsafe { libc::mprotect(mapping.cast::<u8>().add(page).cast(), page, libc::PROT_NONE) },
        0
    );
    assert_eq!(
        unsafe { libc::fchmodat(directory.as_raw_fd(), pointer.cast(), 0o700, 0) },
        0
    );
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_fchmodat,
                directory.as_raw_fd(),
                mapping.cast::<u8>().add(page),
                0o700,
            )
        },
        -1
    );
    assert_eq!(unsafe { libc::munmap(mapping, page * 2) }, 0);
}

fn descriptor_race(root: &Path) {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let path = root.join("scratch/race");
    fs::write(&path, "owned").unwrap();
    let inside = File::open(&path).unwrap();
    let outside = File::open(root.join("source/code")).unwrap();
    let switched = inside.try_clone().unwrap();
    let fd = switched.as_raw_fd();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let thread = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            assert_eq!(unsafe { libc::dup2(outside.as_raw_fd(), fd) }, fd);
            assert_eq!(unsafe { libc::dup2(inside.as_raw_fd(), fd) }, fd);
        }
    });
    for _ in 0..1000 {
        let result = unsafe { libc::fchmod(fd, 0o700) };
        assert!(result == 0 || result == -1);
    }
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("source/code")).unwrap(),
        "original"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn confined_terminal_and_broker_teardown_share_the_reactor() {
    let host = support::Host::new();
    let mut p = policy(&host);
    p.read_only.push(support::task_worker().to_path_buf());
    let sandbox = Sandbox::prepare(p).unwrap();
    let mut plan = sandbox.command(support::task_worker()).unwrap();
    plan.command().arg("terminal");
    let (terminal, mut process) = plan.spawn_terminal(24, 80).unwrap();
    let exit = ChildExit::new(process.child()).unwrap();
    let mut buffer = [0; 512];
    let count = tokio::time::timeout(Duration::from_secs(2), terminal.read(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&buffer[..count]).contains("READY"));
    terminal.interrupt().unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), exit.wait())
            .await
            .unwrap()
            .unwrap()
            .signal(),
        Some(libc::SIGINT)
    );
    assert_eq!(process.stop().await.unwrap().signal(), Some(libc::SIGINT));
    assert_eq!(process.stop().await.unwrap().signal(), Some(libc::SIGINT));
}
