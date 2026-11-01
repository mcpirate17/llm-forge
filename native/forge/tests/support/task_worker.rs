//! Standalone Rust process fixture; compiled by the integration-test support module.

use std::io::{self, Write};
use std::time::Duration;

fn argument_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn duration(args: &[String]) -> io::Result<Duration> {
    args.first()
        .ok_or_else(|| argument_error("missing duration in milliseconds"))?
        .parse()
        .map(Duration::from_millis)
        .map_err(|_| argument_error("invalid duration in milliseconds"))
}

#[cfg(unix)]
fn wait_for_pid(child: &mut std::process::Child, path: &str) -> io::Result<()> {
    let began = std::time::Instant::now();
    loop {
        match std::fs::read_to_string(path) {
            Ok(raw) if raw.parse::<u32>() == Ok(child.id()) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "detached child exited before writing its PID",
            ));
        }
        if began.elapsed() >= Duration::from_secs(3) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "detached child did not write its PID",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn detached_child(args: &[String], exit_after_ready: bool) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let sleep = duration(args)?;
    let path = args
        .get(1)
        .ok_or_else(|| argument_error("missing detached child PID path"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["pid-sleep", &args[0], path])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if exit_after_ready {
        let readiness = wait_for_pid(&mut child, path);
        if readiness.is_err() && child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
        // On success, leave the descendant for the supervisor's cgroup cleanup.
        return readiness;
    }
    std::thread::sleep(sleep);
    child.wait()?;
    Ok(())
}

fn allocate(args: &[String]) -> io::Result<()> {
    let mib: usize = args
        .first()
        .ok_or_else(|| argument_error("missing allocation size in MiB"))?
        .parse()
        .map_err(|_| argument_error("invalid allocation size in MiB"))?;
    if mib > 128 {
        return Err(argument_error(
            "allocation exceeds fixture limit of 128 MiB",
        ));
    }
    let mut memory = vec![0u8; mib * 1024 * 1024];
    for page in memory.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&memory);
    Ok(())
}

fn program_name() -> Option<String> {
    let program = std::env::args_os().next().unwrap_or_default();
    std::path::Path::new(&program)
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

#[cfg(unix)]
fn slow_cleanup_probe() -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    if std::env::var("FORGE_TEST_SLOW_CLEANUP").as_deref() != Ok("1")
        || program_name().as_deref() != Some("systemctl")
    {
        return Ok(());
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--property=LoadState") {
        std::thread::sleep(Duration::from_millis(2200));
    }
    Err(std::process::Command::new("/usr/bin/systemctl")
        .args(args)
        .exec())
}

fn preflight_probe() -> io::Result<Option<i32>> {
    let Some(pid_path) = std::env::var_os("FORGE_TEST_PREFLIGHT_PROBE_PID") else {
        return Ok(None);
    };
    let args: Vec<_> = std::env::args().skip(1).collect();
    match program_name().as_deref() {
        Some("systemctl") => {
            if args.iter().any(|arg| arg == "--property=Version") {
                std::fs::write(pid_path, std::process::id().to_string())?;
                std::thread::sleep(Duration::from_secs(1));
                println!("255");
            }
            Ok(Some(0))
        }
        Some("systemd-run") if args == ["--version"] => {
            println!("systemd 255");
            Ok(Some(0))
        }
        Some("systemd-run") => Err(io::Error::other(
            "task launch reached fake backend after preflight cancellation",
        )),
        _ => Ok(None),
    }
}

#[cfg(unix)]
fn gpu_probe() -> io::Result<Option<i32>> {
    let Some(pid_path) = std::env::var_os("FORGE_TEST_GPU_PROBE_PID") else {
        return Ok(None);
    };
    if program_name().as_deref() != Some("nvidia-smi") {
        return Ok(None);
    }
    let path = pid_path
        .to_str()
        .ok_or_else(|| argument_error("GPU probe PID path is not UTF-8"))?;
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["pid-sleep", "20000", path])
        .spawn()?;
    if let Err(error) = wait_for_pid(&mut child, path) {
        if child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
        return Err(error);
    }
    // The successful probe exits first; its descendant retains the same process group.
    println!("0,GPU-aaaaaaaa-1111-2222-3333-444444444444,1024,1024,0");
    io::stdout().flush()?;
    Ok(Some(0))
}

fn session_start_extension() -> io::Result<i32> {
    use std::io::Read;
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let dir = std::env::current_exe()?.parent().unwrap().to_owned();
    std::fs::write(dir.join("received.json"), input)?;
    match std::fs::read_to_string(dir.join("mode"))?.as_str() {
        "fail" => {
            eprintln!("fixture extension failure");
            Ok(7)
        }
        "sleep" => {
            std::thread::sleep(Duration::from_secs(10));
            Ok(0)
        }
        "flood" => {
            print!("{}", "x".repeat(32768));
            Ok(0)
        }
        _ => {
            println!("Project extension context 🦀");
            Ok(0)
        }
    }
}
fn run() -> io::Result<i32> {
    if program_name().as_deref() == Some("session-start") {
        return session_start_extension();
    }
    #[cfg(unix)]
    slow_cleanup_probe()?;
    if let Some(code) = preflight_probe()? {
        return Ok(code);
    }
    #[cfg(unix)]
    if let Some(code) = gpu_probe()? {
        return Ok(code);
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (mode, args) = args
        .split_first()
        .ok_or_else(|| argument_error("missing worker mode"))?;
    match mode.as_str() {
        "output" => {
            io::stdout().write_all(&[b'x'; 8192])?;
            io::stderr().write_all(b"error detail")?;
        }
        "echo" => {
            for argument in args {
                writeln!(io::stdout(), "{argument}")?;
            }
        }
        "stdin-duplex" => {
            use std::io::Read;
            io::stdout().write_all(&vec![b'p'; 256 * 1024])?;
            io::stdout().flush()?;
            let mut input = Vec::new();
            io::stdin().take(1024 * 1024 + 1).read_to_end(&mut input)?;
            io::stderr().write_all(b"stdin consumed\n")?;
            io::stdout().write_all(&input)?;
        }
        "stdin-echo" => {
            io::copy(&mut io::stdin(), &mut io::stdout())?;
        }
        "allocate" => allocate(args)?,
        #[cfg(target_os = "linux")]
        "flood-both" => flood_both(args)?,
        #[cfg(target_os = "linux")]
        "map-probe" => map_probe(args)?,
        #[cfg(target_os = "linux")]
        "resource-limits" => resource_limits()?,
        "process-limit" => process_limit()?,
        #[cfg(target_os = "linux")]
        "terminal" => return terminal(),
        #[cfg(target_os = "linux")]
        "close-terminal" => return close_terminal(),
        "exit" => {
            return args
                .first()
                .ok_or_else(|| argument_error("missing exit code"))?
                .parse()
                .map_err(|_| argument_error("invalid exit code"));
        }
        "sleep" => std::thread::sleep(duration(args)?),
        "burn" => {
            let until = std::time::Instant::now() + duration(args)?;
            while std::time::Instant::now() < until {
                std::hint::spin_loop();
            }
        }
        "pid-sleep" => {
            let sleep = duration(args)?;
            let path = args.get(1).map(String::as_str).unwrap_or("child.pid");
            std::fs::write(path, std::process::id().to_string())?;
            std::thread::sleep(sleep);
        }
        "env" => {
            for key in args {
                let value = std::env::var(key)
                    .map_err(|error| argument_error(&format!("{key}: {error}")))?;
                println!("{key}={value}");
            }
        }
        "inspect-cgroup" => {
            io::stdout().write_all(&std::fs::read("/proc/self/cgroup")?)?;
        }
        #[cfg(unix)]
        "detach-child" => detached_child(args, false)?,
        #[cfg(unix)]
        "detach-exit" => detached_child(args, true)?,
        _ => return Err(argument_error(&format!("unknown worker mode: {mode}"))),
    }
    io::stdout().flush()?;
    io::stderr().flush()?;
    Ok(0)
}

#[cfg(target_os = "linux")]
fn flood_both(args: &[String]) -> io::Result<()> {
    let until = std::time::Instant::now() + duration(args)?;
    let writer = std::thread::spawn(move || -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        while std::time::Instant::now() < until {
            stdout.write_all(&[b'o'; 8192])?;
        }
        Ok(())
    });
    let mut stderr = io::stderr().lock();
    while std::time::Instant::now() < until {
        stderr.write_all(&[b'e'; 8192])?;
    }
    writer
        .join()
        .map_err(|_| io::Error::other("stdout writer panicked"))??;
    Ok(())
}
#[cfg(target_os = "linux")]
fn map_probe(args: &[String]) -> io::Result<()> {
    use std::ffi::c_void;
    extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }
    let mib: usize = args
        .first()
        .ok_or_else(|| argument_error("missing mapping size"))?
        .parse()
        .map_err(|_| argument_error("invalid mapping size"))?;
    if mib > 128 {
        return Err(argument_error("mapping exceeds fixture limit"));
    }
    let bytes = mib * 1024 * 1024;
    // Anonymous private mapping is reserved but never touched: it tests address
    // space without charging 96 MiB of physical cgroup memory.
    let address = unsafe { mmap(std::ptr::null_mut(), bytes, 3, 0x22, -1, 0) };
    if address as isize == -1 {
        println!("mapping denied");
    } else {
        println!("mapping granted");
        if unsafe { munmap(address, bytes) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn resource_limits() -> io::Result<()> {
    #[repr(C)]
    struct Rlimit {
        current: std::os::raw::c_ulong,
        maximum: std::os::raw::c_ulong,
    }
    extern "C" {
        fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
    }
    let mut cpu = Rlimit {
        current: 0,
        maximum: 0,
    };
    let mut address = Rlimit {
        current: 0,
        maximum: 0,
    };
    // Linux resource IDs RLIMIT_CPU=0 and RLIMIT_AS=9; the fixture uses libc ABI.
    if unsafe { getrlimit(0, &mut cpu) } != 0 || unsafe { getrlimit(9, &mut address) } != 0 {
        return Err(io::Error::last_os_error());
    }
    println!("cpu={} as={}", cpu.current, address.current);
    Ok(())
}

fn process_limit() -> io::Result<()> {
    match std::process::Command::new(std::env::current_exe()?)
        .args(["exit", "0"])
        .spawn()
    {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(error),
        Ok(mut child) => {
            child.wait()?;
            Err(io::Error::other(
                "process-count limit did not prevent child creation",
            ))
        }
    }
}

#[cfg(target_os = "linux")]
fn close_terminal() -> io::Result<i32> {
    extern "C" {
        fn close(fd: i32) -> i32;
    }
    println!("STDIO-CLOSING");
    io::stdout().flush()?;
    for fd in 0..3 {
        // SAFETY: the fixture relinquishes its own standard descriptors only.
        if unsafe { close(fd) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    Ok(0)
}

#[cfg(target_os = "linux")]
fn terminal() -> io::Result<i32> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return Err(io::Error::other("all standard streams must be terminals"));
    }
    extern "C" {
        fn tcgetsid(fd: i32) -> i32;
        fn tcgetpgrp(fd: i32) -> i32;
        fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct Size {
        rows: u16,
        columns: u16,
        x: u16,
        y: u16,
    }
    // SAFETY: these calls only inspect the fixture's own standard terminal.
    if unsafe { tcgetsid(0) } != std::process::id() as i32
        || unsafe { tcgetpgrp(0) } != std::process::id() as i32
    {
        return Err(io::Error::other(
            "worker does not own its controlling terminal",
        ));
    }
    println!("READY");
    io::stdout().flush()?;
    let mut line = String::new();
    while io::stdin().read_line(&mut line)? != 0 {
        match line.trim() {
            "exit" => return Ok(0),
            "size" => {
                let mut size = Size::default();
                // SAFETY: Linux TIOCGWINSZ writes a repr(C) four-u16 winsize.
                if unsafe { ioctl(0, 0x5413, &mut size) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                println!("SIZE {} {}", size.rows, size.columns);
            }
            _ => println!("ECHO {}", line.trim_end()),
        }
        io::stdout().flush()?;
        line.clear();
    }
    Ok(0)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("task worker: {error}");
            std::process::exit(2);
        }
    }
}
