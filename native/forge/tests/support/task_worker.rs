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

fn run() -> io::Result<i32> {
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
        "allocate" => allocate(args)?,
        "exit" => {
            return args
                .first()
                .ok_or_else(|| argument_error("missing exit code"))?
                .parse()
                .map_err(|_| argument_error("invalid exit code"));
        }
        "sleep" => std::thread::sleep(duration(args)?),
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

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("task worker: {error}");
            std::process::exit(2);
        }
    }
}
