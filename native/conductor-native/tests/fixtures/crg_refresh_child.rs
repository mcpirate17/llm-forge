//! Test-only process fixture for real graph-refresh worker and batch behavior.

use pyo3::exceptions::{PyOSError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn hold_lock(file: &Path, seconds: f64) -> Result<(), Box<dyn std::error::Error>> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(file)?;
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    std::thread::sleep(Duration::from_secs_f64(seconds));
    Ok(())
}

fn drain(store_root: &Path, output: &Path) -> PyResult<()> {
    Python::initialize();
    Python::attach(|py| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
        py.import("sys")?
            .getattr("path")?
            .call_method1("insert", (0, source.to_str().expect("UTF-8 source path")))?;
        let pathlib = py.import("pathlib")?;
        let root = pathlib
            .getattr("Path")?
            .call1((store_root.to_str().unwrap(),))?;
        let refresh = py.import("tooling.hooks.agent.crg_refresh_state")?;
        let store = refresh.getattr("Store")?.call1((root,))?;
        let destination = output.to_path_buf();
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                if args.len() != 1 || kwargs.is_some_and(|value| !value.is_empty()) {
                    return Err(PyTypeError::new_err("refresh expects one batch"));
                }
                let paths: Vec<String> = args.get_item(0)?.extract()?;
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&destination)
                    .map_err(|error| PyOSError::new_err(error.to_string()))?;
                writeln!(file, "{}", paths.join(","))
                    .map_err(|error| PyOSError::new_err(error.to_string()))
            },
        )?;
        let options = PyDict::new(py);
        options.set_item("debounce", 0.05)?;
        refresh
            .getattr("drain")?
            .call((store, callback), Some(&options))?;
        Ok(())
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.as_slice() {
        [_, mode, file, seconds] if mode == "hold-lock" => {
            hold_lock(Path::new(file), seconds.parse()?)?
        }
        [_, mode, root, output] if mode == "drain" => drain(Path::new(root), Path::new(output))?,
        [_, mode, seconds] if mode == "sleep" => {
            std::thread::sleep(Duration::from_secs_f64(seconds.parse()?));
        }
        [_, mode] if mode == "warn" => {
            println!("embedding bridge unavailable: semantic search is stale");
        }
        [_, mode] if mode == "fail" => {
            eprintln!("code-review-graph is not installed; graph NOT refreshed");
            std::process::exit(1);
        }
        [_, mode, marker] if mode == "write-marker" => {
            std::fs::write(PathBuf::from(marker), "x")?;
        }
        _ => {
            return Err("expected hold-lock|drain|sleep|warn|fail|write-marker fixture mode".into())
        }
    }
    Ok(())
}
