//! Isolated, Rust-controlled child for the installed-wheel resource contracts.

use pyo3::exceptions::{PyOSError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyList, PyModule};
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::{CStr, CString};
use std::fs::{self, File, FileTimes};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Deserialize)]
struct Request {
    package: String,
    name: String,
    site: PathBuf,
    hook: Hook,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Hook {
    None,
    AppendPath {
        path: PathBuf,
    },
    ShadowPath {
        path: PathBuf,
    },
    ReplaceAfterOpen {
        resource: PathBuf,
        replacement: PathBuf,
    },
    FifoBeforeOpen {
        resource: PathBuf,
    },
    SymlinkBeforeOpen {
        resource: PathBuf,
        target: PathBuf,
    },
    IntermediateBeforeOpen {
        nested: PathBuf,
        external: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let interpreter = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("missing venv interpreter")?,
    );
    let mut request = Vec::new();
    std::io::stdin().read_to_end(&mut request)?;
    let request: Request = serde_json::from_slice(&request)?;
    initialize_venv(&interpreter)?;
    let result = Python::attach(|py| read(py, &interpreter, &request));
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            Python::attach(|py| error.print(py));
            return Err("installed-wheel resource child failed".into());
        }
    }
    Ok(())
}

fn check_status(status: pyo3::ffi::PyStatus) -> Result<(), String> {
    if unsafe { pyo3::ffi::PyStatus_Exception(status) } == 0 {
        return Ok(());
    }
    let message = if status.err_msg.is_null() {
        "Python initialization failed".into()
    } else {
        unsafe { CStr::from_ptr(status.err_msg) }
            .to_string_lossy()
            .into_owned()
    };
    Err(message)
}

fn initialize_venv(interpreter: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let name = CString::new(interpreter.as_os_str().as_bytes())?;
    unsafe {
        let mut config = std::mem::MaybeUninit::<pyo3::ffi::PyConfig>::uninit();
        pyo3::ffi::PyConfig_InitIsolatedConfig(config.as_mut_ptr());
        let mut config = config.assume_init();
        config.write_bytecode = 0;
        config.parse_argv = 0;
        let result = check_status(pyo3::ffi::PyConfig_SetBytesString(
            &mut config,
            &mut config.program_name,
            name.as_ptr(),
        ))
        .and_then(|()| {
            check_status(pyo3::ffi::PyConfig_SetBytesString(
                &mut config,
                &mut config.executable,
                name.as_ptr(),
            ))
        })
        .and_then(|()| check_status(pyo3::ffi::Py_InitializeFromConfig(&config)));
        pyo3::ffi::PyConfig_Clear(&mut config);
        result?;
    }
    Ok(())
}

fn read(py: Python<'_>, interpreter: &Path, request: &Request) -> PyResult<Value> {
    verify_isolated_venv(py, interpreter, &request.site)?;
    let sys = PyModule::import(py, "sys")?;
    let path = sys.getattr("path")?.cast_into::<PyList>()?;
    match &request.hook {
        Hook::AppendPath { path: extra } => path.append(extra.to_string_lossy().as_ref())?,
        Hook::ShadowPath { path: extra } => path.insert(0, extra.to_string_lossy().as_ref())?,
        _ => {}
    }
    let resources = PyModule::import(py, "conductor.package_resources")?;
    let actual_source: String = resources.getattr("__file__")?.extract()?;
    let expected_source = request.site.join("conductor/package_resources.py");
    if Path::new(&actual_source) != expected_source {
        return Err(PyRuntimeError::new_err(format!(
            "resource reader came from {}, expected {}",
            actual_source,
            expected_source.display()
        )));
    }
    let context = PyModule::import(py, "conductor.project_context")?;
    let event = install_hook(py, &resources, &context, &request.hook)?;
    let outcome = resources
        .getattr("read_package_resource")?
        .call1((&request.package, &request.name));
    if let Some((called, expected)) = event {
        if called.load(Ordering::SeqCst) != expected {
            return Err(PyRuntimeError::new_err(
                "resource callback invocation drift",
            ));
        }
    }
    match outcome {
        Ok(result) => Ok(json!({
            "package": result.getattr("package")?.extract::<String>()?,
            "name": result.getattr("name")?.extract::<String>()?,
            "version": result.getattr("distribution_version")?.extract::<String>()?,
            "data": String::from_utf8(result.getattr("data")?.extract::<Vec<u8>>()?)
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?,
            "sha256": result.getattr("sha256")?.extract::<String>()?,
        })),
        Err(error) if error.is_instance(py, &context.getattr("ContextError")?) => {
            let detail = error.value(py).getattr("detail")?;
            Ok(json!({
                "code": detail.getattr("code")?.extract::<String>()?,
                "field": detail.getattr("field")?.extract::<Option<String>>()?,
            }))
        }
        Err(error) => Err(error),
    }
}

fn verify_isolated_venv(py: Python<'_>, interpreter: &Path, site: &Path) -> PyResult<()> {
    let sys = PyModule::import(py, "sys")?;
    let expected = interpreter
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| PyRuntimeError::new_err("interpreter is outside a venv"))?;
    let prefix: String = sys.getattr("prefix")?.extract()?;
    if Path::new(&prefix) != expected || !site.starts_with(expected) {
        return Err(PyRuntimeError::new_err(format!(
            "resource child escaped venv: prefix={prefix}, expected={}",
            expected.display()
        )));
    }
    let paths = sys.getattr("path")?.cast_into::<PyList>()?;
    let installed = paths
        .iter()
        .filter_map(|entry| entry.extract::<String>().ok())
        .any(|entry| Path::new(&entry) == site);
    if !installed {
        return Err(PyRuntimeError::new_err(
            "private site-packages absent from sys.path",
        ));
    }
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(io_error)?;
    if paths
        .iter()
        .filter_map(|entry| entry.extract::<String>().ok())
        .any(|entry| Path::new(&entry).starts_with(&checkout))
    {
        return Err(PyRuntimeError::new_err(
            "Forge checkout leaked into sys.path",
        ));
    }
    Ok(())
}

fn install_hook(
    py: Python<'_>,
    resources: &Bound<'_, PyModule>,
    context: &Bound<'_, PyModule>,
    hook: &Hook,
) -> PyResult<Option<(Arc<AtomicBool>, bool)>> {
    match hook {
        Hook::ShadowPath { .. } => {
            let called = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&called);
            let detail = context.getattr("ErrorDetail")?.call1((
                "RESOURCE_UNSAFE",
                "package",
                "untrusted resource backend was invoked",
            ))?;
            let error = context.getattr("ContextError")?.call1((detail,))?.unbind();
            let callback =
                PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<()> {
                    observed.store(true, Ordering::SeqCst);
                    Err(PyErr::from_value(error.bind(args.py()).clone()))
                })?;
            resources.getattr("resources")?.setattr("files", callback)?;
            Ok(Some((called, false)))
        }
        Hook::ReplaceAfterOpen {
            resource,
            replacement,
        } => replace_after_open(py, resources, resource, replacement).map(Some),
        Hook::FifoBeforeOpen { resource } => {
            open_hook(py, resources, OpenSwap::Fifo(resource.clone())).map(Some)
        }
        Hook::SymlinkBeforeOpen { resource, target } => open_hook(
            py,
            resources,
            OpenSwap::Symlink(resource.clone(), target.clone()),
        )
        .map(Some),
        Hook::IntermediateBeforeOpen { nested, external } => open_hook(
            py,
            resources,
            OpenSwap::Intermediate(nested.clone(), external.clone()),
        )
        .map(Some),
        Hook::None | Hook::AppendPath { .. } => Ok(None),
    }
}

fn replace_after_open(
    py: Python<'_>,
    resources: &Bound<'_, PyModule>,
    resource: &Path,
    replacement: &Path,
) -> PyResult<(Arc<AtomicBool>, bool)> {
    let os = resources.getattr("os")?;
    let original = os.getattr("fstat")?.unbind();
    let resource = resource.to_path_buf();
    let replacement = replacement.to_path_buf();
    let parent = resource.parent().expect("resource parent").to_path_buf();
    let metadata = fs::metadata(&parent).map_err(io_error)?;
    let times = FileTimes::new()
        .set_accessed(metadata.accessed().map_err(io_error)?)
        .set_modified(metadata.modified().map_err(io_error)?);
    let called = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&called);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let stat = original.bind(args.py()).call(args, kwargs)?;
        let mode: u32 = stat.getattr("st_mode")?.extract()?;
        if mode & libc::S_IFMT == libc::S_IFREG && !observed.swap(true, Ordering::SeqCst) {
            fs::rename(&replacement, &resource).map_err(io_error)?;
            File::open(&parent)
                .and_then(|file| file.set_times(times))
                .map_err(io_error)?;
        }
        Ok::<_, PyErr>(stat.unbind())
    })?;
    os.setattr("fstat", callback)?;
    Ok((called, true))
}

enum OpenSwap {
    Fifo(PathBuf),
    Symlink(PathBuf, PathBuf),
    Intermediate(PathBuf, PathBuf),
}

fn open_hook(
    py: Python<'_>,
    resources: &Bound<'_, PyModule>,
    swap: OpenSwap,
) -> PyResult<(Arc<AtomicBool>, bool)> {
    let os = resources.getattr("os")?;
    let original = os.getattr("open")?.unbind();
    let called = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&called);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let anchored = kwargs
            .and_then(|kw| kw.get_item("dir_fd").transpose())
            .transpose()?
            .is_some_and(|fd| !fd.is_none());
        if anchored && !observed.load(Ordering::SeqCst) {
            match &swap {
                OpenSwap::Fifo(resource) => {
                    fs::remove_file(resource).map_err(io_error)?;
                    let name = CString::new(resource.as_os_str().as_bytes())
                        .map_err(|error| PyOSError::new_err(error.to_string()))?;
                    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                        return Err(io_error(std::io::Error::last_os_error()));
                    }
                    observed.store(true, Ordering::SeqCst);
                }
                OpenSwap::Symlink(resource, target) => {
                    fs::remove_file(resource).map_err(io_error)?;
                    symlink(target, resource).map_err(io_error)?;
                    observed.store(true, Ordering::SeqCst);
                }
                OpenSwap::Intermediate(nested, external) => {
                    let path = args.get_item(0)?.str()?.to_str()?.to_owned();
                    if path == "nested" {
                        fs::remove_file(nested.join("fixture.txt")).map_err(io_error)?;
                        fs::remove_dir(nested).map_err(io_error)?;
                        symlink(external, nested).map_err(io_error)?;
                        observed.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        Ok::<_, PyErr>(original.bind(args.py()).call(args, kwargs)?.unbind())
    })?;
    os.setattr("open", callback)?;
    Ok((called, true))
}

fn io_error(error: std::io::Error) -> PyErr {
    PyOSError::new_err(error.to_string())
}
