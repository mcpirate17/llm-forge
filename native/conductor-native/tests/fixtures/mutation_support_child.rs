//! Test-only child processes for mutation command lifetime contracts.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{self, Command};

fn engine(registry: &Path, finished: &Path, cwd: &Path) -> PyResult<()> {
    Python::initialize();
    Python::attach(|py| -> PyResult<()> {
        let support = py.import("conductor.mutation_testing_support")?;
        let pathlib = py.import("pathlib")?;
        let builtins = py.import("builtins")?;
        let args = PyList::new(py, ["sleep", "300"])?;
        let kw = PyDict::new(py);
        kw.set_item(
            "cwd",
            pathlib.getattr("Path")?.call1((cwd.to_string_lossy(),))?,
        )?;
        kw.set_item("timeout_seconds", 300)?;
        kw.set_item("environment", PyDict::new(py))?;
        kw.set_item("pin_argv", builtins.getattr("list")?)?;
        kw.set_item("result_factory", builtins.getattr("dict")?)?;
        kw.set_item("output_tail_chars", 10)?;
        kw.set_item(
            "pgid_registry",
            pathlib
                .getattr("Path")?
                .call1((registry.to_string_lossy(),))?,
        )?;
        support.getattr("run_command")?.call((args,), Some(&kw))?;
        Ok(())
    })?;
    fs::write(finished, "done")?;
    Ok(())
}

fn leaderless(marker: &Path) -> io::Result<()> {
    let child = Command::new("sleep").arg("300").spawn()?;
    fs::write(marker, child.id().to_string())
}

fn pdeathsig() -> io::Result<()> {
    let mut value: libc::c_int = 0;
    let status = unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &mut value) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    println!("{value}");
    Ok(())
}

fn main() {
    let args: Vec<_> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("engine") if args.len() == 5 => engine(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        )
        .map_err(|error| error.to_string()),
        Some("leaderless") if args.len() == 3 => {
            leaderless(Path::new(&args[2])).map_err(|error| error.to_string())
        }
        Some("pdeathsig") if args.len() == 2 => pdeathsig().map_err(|error| error.to_string()),
        _ => Err("expected engine REGISTRY FINISHED CWD, leaderless MARKER, or pdeathsig".into()),
    };
    if let Err(error) = result {
        eprintln!("mutation_support_child: {error}");
        process::exit(2);
    }
}
