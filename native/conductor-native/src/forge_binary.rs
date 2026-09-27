//! Select the Forge executable associated with a host's Python interpreter.
//!
//! An interpreter inside a venv is often a symlink to a system Python. Keep
//! its lexical parent directory: the installed `forge` script lives beside
//! that symlink, not beside the resolved interpreter.

use std::env;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[cfg(feature = "python")]
use pyo3::exceptions::PyRuntimeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // `os.access(path, os.X_OK)` is the prior Python contract. Mode-bit
    // inspection differs for ACLs and the process's actual uid/gid.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if name.contains('/') {
        return executable_file(path).then(|| path.to_path_buf());
    }
    for directory in env::split_paths(&env::var_os("PATH")?) {
        let candidate = directory.join(name);
        if executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Select one executable without resolving the interpreter's symlink.
///
/// `path_forge` is the caller's `shutil.which("forge")` observation. Preserve
/// that observation as the final fallback, including its returned path form.
pub fn resolve_forge_binary(
    project_dir: &Path,
    python_executable: &Path,
    configured: Option<&str>,
    path_forge: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    if let Some(value) = configured.filter(|value| !value.is_empty()) {
        return executable_on_path(value)
            .map(Some)
            .ok_or_else(|| format!("FORGE_BIN does not resolve to an executable: {value}"));
    }

    if let Some(parent) = python_executable.parent() {
        let installed = parent.join("forge");
        if executable_file(&installed) {
            return Ok(Some(installed));
        }
    }

    let local = project_dir.join(".tools/bin/forge");
    if executable_file(&local) {
        return Ok(Some(local));
    }
    Ok(path_forge
        .filter(|value| !value.is_empty())
        .map(PathBuf::from))
}

#[cfg(feature = "python")]
#[pyfunction]
fn resolve_forge_binary_native(
    project_dir: &str,
    python_executable: &str,
    configured: Option<&str>,
    path_forge: Option<&str>,
) -> PyResult<Option<String>> {
    resolve_forge_binary(
        Path::new(project_dir),
        Path::new(python_executable),
        configured,
        path_forge,
    )
    .map(|selected| selected.map(|path| path.to_string_lossy().into_owned()))
    .map_err(PyRuntimeError::new_err)
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(resolve_forge_binary_native, module)?)?;
    Ok(())
}
