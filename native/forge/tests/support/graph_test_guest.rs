//! Native supervisor fixture, deliberately not a compiler or Python capability test.
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
extern "C" {
    fn raise(signal: i32) -> i32;
}
fn parent(pid: u32) -> io::Result<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .unwrap()
        .trim()
        .parse()
        .map_err(io::Error::other)
}
fn observed(root: &Path, name: &str, value: u32) -> io::Result<()> {
    fs::write(root.join(name), value.to_string())
}
fn sleeping(root: &Path, direct: bool) -> io::Result<()> {
    if std::env::var_os("FORGE_GRAPH_FIXTURE_DESCENDANT").is_some() {
        observed(root, "descendant.pid", std::process::id())?;
        std::thread::sleep(Duration::from_secs(60));
        return Ok(());
    }
    let mut child = Command::new(std::env::current_exe()?)
        .env("FORGE_GRAPH_FIXTURE_DESCENDANT", "1")
        .spawn()?;
    observed(root, "leader.pid", std::process::id())?;
    let supervisor = if direct {
        parent(std::process::id())?
    } else {
        parent(parent(std::process::id())?)?
    };
    observed(root, "supervisor.pid", supervisor)?;
    let began = Instant::now();
    while !root.join("descendant.pid").exists() {
        if began.elapsed() > Duration::from_secs(3) {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::other("descendant never became ready"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(root.join("ready"), "ready")?;
    child.wait()?;
    Ok(())
}
fn build_artifact(args: &[String]) -> io::Result<()> {
    let manifest = &args[args
        .iter()
        .position(|arg| arg == "--manifest-path")
        .unwrap()
        + 1];
    let binary = manifest.ends_with("native/forge/Cargo.toml");
    let name = if binary { "forge" } else { "conductor_native" };
    let directory = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap()).join("debug");
    fs::create_dir_all(&directory)?;
    let file = directory.join(if binary {
        "forge"
    } else {
        "libconductor_native.so"
    });
    fs::copy(std::env::current_exe()?, &file)?;
    let file = file.to_str().unwrap();
    let executable = if binary {
        format!("{file:?}")
    } else {
        "null".into()
    };
    println!("{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":{name:?}}},\"filenames\":[{file:?}],\"executable\":{executable}}}");
    Ok(())
}

fn record_runtime(surface: &str) -> io::Result<()> {
    let root = PathBuf::from(std::env::var_os("FORGE_GRAPH_FIXTURE_OBSERVATIONS").unwrap());
    let runtime = PathBuf::from(std::env::var_os("TMPDIR").unwrap());
    let extension = runtime.join("contract-extension/conductor_native.so");
    let binary = PathBuf::from(
        std::env::var_os("FORGE_BIN").ok_or_else(|| io::Error::other("fresh Forge missing"))?,
    );
    assert_eq!(binary, runtime.join("contract-forge"));
    assert_eq!(fs::read(&extension)?, fs::read(std::env::current_exe()?)?);
    assert_eq!(fs::read(&binary)?, fs::read(std::env::current_exe()?)?);
    let imports =
        std::env::var_os("PYTHONPATH").ok_or_else(|| io::Error::other("fresh imports missing"))?;
    assert_eq!(
        std::env::split_paths(&imports).next().unwrap(),
        runtime.join("contract-extension")
    );
    let identity = format!("{}\n{}\n", extension.display(), binary.display());
    if surface == "pytest" {
        assert_eq!(fs::read_to_string(root.join("contract.runtime"))?, identity);
    }
    fs::write(root.join(format!("{surface}.runtime")), identity)?;
    Ok(())
}

fn main() -> io::Result<()> {
    let mode = std::env::var("FORGE_GRAPH_FIXTURE_MODE").unwrap_or_else(|_| "success".into());
    let args: Vec<_> = std::env::args().skip(1).collect();
    let observations =
        || PathBuf::from(std::env::var_os("FORGE_GRAPH_FIXTURE_OBSERVATIONS").unwrap());
    if args.iter().any(|arg| arg == "-c") {
        if mode == "probe-sleep" {
            return sleeping(&observations(), true);
        }
        println!("native guest prerequisite fixture");
        return Ok(());
    }
    let pytest = args.windows(2).any(|pair| pair == ["-m", "pytest"]);
    if pytest {
        fs::write(observations().join("pytest.argv"), args.join("\0"))?;
    }
    if mode.starts_with("combined") {
        if args.iter().any(|arg| arg == "build") {
            return build_artifact(&args);
        }
        if pytest {
            record_runtime("pytest")?;
            if mode == "combined-pytest-fail" {
                std::process::exit(41);
            }
        } else if args.iter().any(|arg| arg == "--test") {
            record_runtime("contract")?;
        }
        println!("combined native supervisor fixture success");
        return Ok(());
    }
    match mode.as_str() {
        "success" => println!("native supervisor fixture success"),
        "exit" => std::process::exit(37),
        "signal" => {
            unsafe {
                raise(15);
            }
            return Err(io::Error::other("signal did not terminate fixture"));
        }
        "sleep" => sleeping(&observations(), false)?,
        "probe-sleep" => sleeping(&observations(), true)?,
        _ => return Err(io::Error::other("unknown fixture mode")),
    }
    Ok(())
}
