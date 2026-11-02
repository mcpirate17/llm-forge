//! Optional PyO pytest hooks for sampled native mappings during an invariant
//! measurement. They do not prove that no library was loaded between samples.
use anyhow::{ensure, Context, Result};
use pyo3::{
    exceptions::PyRuntimeError,
    prelude::*,
    types::{PyDict, PyModule},
};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
const MAX_MAPS: u64 = 8 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_FOUND: usize = 4096;
struct State {
    root: PathBuf,
    source_root: PathBuf,
    report: PathBuf,
    parent: (u64, u64),
    node: bool,
    nodeid: String,
    torch_version: String,
    torch_origin: PathBuf,
    torch_module: Option<Py<PyAny>>,
    found: BTreeSet<String>,
    samples: usize,
}
static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();
fn state() -> &'static Mutex<Option<State>> {
    STATE.get_or_init(|| Mutex::new(None))
}
fn mapped_path(line: &str) -> Option<&str> {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        let i = rest.find(char::is_whitespace)?;
        rest = &rest[i..];
    }
    let value = rest
        .trim_start()
        .strip_suffix(" (deleted)")
        .unwrap_or(rest.trim_start());
    value.starts_with('/').then_some(value)
}
fn native(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    name.ends_with(".so")
        || name.contains(".so.")
        || name.ends_with(".pyd")
        || name.ends_with(".dylib")
}
fn classified(root: &Path, source_root: &Path, path: &str) -> Result<Option<String>> {
    ensure!(
        path.len() <= 4096 && !path.chars().any(char::is_control),
        "invalid mapped native path"
    );
    if !native(path) {
        return Ok(None);
    }
    let p = Path::new(path);
    if let Ok(relative) = p.strip_prefix(root) {
        return Ok(Some(
            relative
                .to_str()
                .context("native mapping path not UTF-8")?
                .into(),
        ));
    }
    if let Ok(relative) = p.strip_prefix(source_root) {
        return Ok(Some(format!(
            "source-root::{}",
            relative.to_str().context("source mapping path not UTF-8")?
        )));
    }
    if p.components().any(
        |c| matches!(c,std::path::Component::Normal(v) if v=="site-packages" || v=="dist-packages"),
    ) {
        return Ok(None);
    }
    if ["torch_extensions", "triton", "numba"]
        .iter()
        .any(|name| path.contains(name))
    {
        return Ok(Some(format!(
            "compiled-cache::{}",
            p.file_name()
                .and_then(|v| v.to_str())
                .context("native cache name invalid")?
        )));
    }
    Ok(None)
}
fn read_maps(root: &Path, source_root: &Path) -> Result<BTreeSet<String>> {
    let mut bytes = Vec::new();
    File::open("/proc/self/maps")?
        .take(MAX_MAPS + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_MAPS,
        "process mappings exceed limit"
    );
    let text = std::str::from_utf8(&bytes).context("process mappings not UTF-8")?;
    let mut found = BTreeSet::new();
    let mut count = 0usize;
    for line in text.lines() {
        count += 1;
        ensure!(count <= MAX_ROWS, "too many process mappings");
        if let Some(path) = mapped_path(line) {
            if let Some(value) = classified(root, source_root, path)? {
                found.insert(value);
            }
        }
        ensure!(found.len() <= MAX_FOUND, "too many native mappings");
    }
    Ok(found)
}
fn inspect(py: Python<'_>, current: &mut State) -> PyResult<()> {
    current.samples = current
        .samples
        .checked_add(1)
        .ok_or_else(|| PyRuntimeError::new_err("native probe sample overflow"))?;
    if current.samples > 16 {
        return Err(PyRuntimeError::new_err(
            "native probe sample limit exceeded",
        ));
    }
    let mappings = read_maps(&current.root, &current.source_root)
        .map_err(|e| PyRuntimeError::new_err(format!("native probe: {e:#}")))?;
    current.found.extend(mappings);
    let modules = PyModule::import(py, "sys")?.getattr("modules")?;
    let modules = modules.cast::<PyDict>()?;
    inspect_torch(modules, current)?;
    for name in ["triton", "numba"] {
        if modules.contains(name)? {
            current.found.insert(format!("opaque-compiler::{name}"));
        }
    }
    if current.found.len() > MAX_FOUND {
        return Err(PyRuntimeError::new_err(
            "native probe mapping limit exceeded",
        ));
    }
    Ok(())
}
fn inspect_torch(modules: &Bound<'_, PyDict>, current: &mut State) -> PyResult<()> {
    let module = modules.get_item("torch")?;
    let Some(module) = module else {
        if current.torch_module.is_some() {
            return Err(PyRuntimeError::new_err("torch module disappeared"));
        }
        return Ok(());
    };
    if current
        .torch_module
        .as_ref()
        .is_some_and(|old| old.as_ptr() != module.as_ptr())
    {
        return Err(PyRuntimeError::new_err("torch module identity changed"));
    }
    let version: String = module.getattr("__version__")?.extract()?;
    let origin: String = module.getattr("__file__")?.extract()?;
    let origin = Path::new(&origin)
        .canonicalize()
        .map_err(|_| PyRuntimeError::new_err("torch origin unavailable"))?;
    if version != current.torch_version
        || origin != current.torch_origin
        || origin.starts_with(&current.root)
        || origin.starts_with(&current.source_root)
    {
        return Err(PyRuntimeError::new_err(
            "torch module does not match isolated installation",
        ));
    }
    if current.torch_module.is_none() {
        current.torch_module = Some(module.unbind());
    }
    Ok(())
}
fn with_state(py: Python<'_>) -> PyResult<()> {
    let mut guard = state()
        .lock()
        .map_err(|_| PyRuntimeError::new_err("native probe state poisoned"))?;
    inspect(
        py,
        guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("native probe was not configured"))?,
    )
}
fn configure() -> Result<State> {
    let root = PathBuf::from(
        std::env::var_os("FORGE_INVARIANT_ROOT").context("native probe root missing")?,
    );
    let report = PathBuf::from(
        std::env::var_os("FORGE_INVARIANT_NATIVE_REPORT").context("native probe report missing")?,
    );
    let source_root = PathBuf::from(
        std::env::var_os("FORGE_INVARIANT_SOURCE_ROOT")
            .context("native probe source root missing")?,
    );
    let phase = std::env::var("FORGE_INVARIANT_PHASE").context("native probe phase missing")?;
    let nodeid = std::env::var("FORGE_INVARIANT_NODEID").context("native probe nodeid missing")?;
    let torch_version =
        std::env::var("FORGE_INVARIANT_TORCH_VERSION").context("torch version missing")?;
    let torch_origin = PathBuf::from(
        std::env::var_os("FORGE_INVARIANT_TORCH_ORIGIN").context("torch origin missing")?,
    );
    ensure!(
        !torch_version.is_empty()
            && torch_version.len() <= 256
            && !torch_version.chars().any(char::is_control)
            && torch_origin.is_absolute()
            && torch_origin.as_os_str().as_encoded_bytes().len() <= 4096
            && torch_origin.canonicalize()? == torch_origin
            && !torch_origin.starts_with(&root)
            && !torch_origin.starts_with(&source_root),
        "invalid isolated torch identity"
    );
    ensure!(
        !nodeid.is_empty() && nodeid.len() <= 4096 && !nodeid.chars().any(char::is_control),
        "invalid native probe nodeid"
    );
    ensure!(
        matches!(phase.as_str(), "baseline" | "node"),
        "invalid native probe phase"
    );
    ensure!(
        root.is_absolute()
            && root.as_os_str().as_encoded_bytes().len() <= 4096
            && root.canonicalize()? == root
            && root.is_dir(),
        "invalid native probe root"
    );
    ensure!(
        source_root.is_absolute()
            && source_root.as_os_str().as_encoded_bytes().len() <= 4096
            && source_root.canonicalize()? == source_root
            && source_root.is_dir(),
        "invalid native probe source root"
    );
    ensure!(
        report.is_absolute()
            && report
                .file_name()
                .is_some_and(|v| v == "invariant-native.json"),
        "invalid native probe report"
    );
    let parent = report.parent().context("native probe parent missing")?;
    ensure!(
        parent.canonicalize()? == parent && !parent.starts_with(&root) && !root.starts_with(parent),
        "native probe report parent invalid"
    );
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o777 == 0o700,
        "native probe parent unsafe"
    );
    Ok(State {
        root,
        source_root,
        report,
        parent: (meta.dev(), meta.ino()),
        node: phase == "node",
        nodeid,
        torch_version,
        torch_origin,
        torch_module: None,
        found: BTreeSet::new(),
        samples: 0,
    })
}
fn save(current: &State) -> Result<()> {
    let parent = current
        .report
        .parent()
        .context("native report parent missing")?;
    let meta = fs::symlink_metadata(parent)?;
    ensure!(
        meta.is_dir()
            && (meta.dev(), meta.ino()) == current.parent
            && parent.canonicalize()? == parent,
        "native report parent changed"
    );
    let bytes = serde_json::to_vec(&current.found)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "native probe report exceeds limit"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&current.report)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
#[pyfunction]
#[pyo3(signature=(config))]
fn pytest_configure(py: Python<'_>, config: &Bound<'_, PyAny>) -> PyResult<()> {
    let _ = config;
    let mut guard = state()
        .lock()
        .map_err(|_| PyRuntimeError::new_err("native probe state poisoned"))?;
    if guard.is_some() {
        return Err(PyRuntimeError::new_err("native probe configured twice"));
    }
    let mut current =
        configure().map_err(|e| PyRuntimeError::new_err(format!("native probe: {e:#}")))?;
    inspect(py, &mut current)?;
    *guard = Some(current);
    Ok(())
}
#[pyfunction]
#[pyo3(signature=(session))]
fn pytest_sessionstart(py: Python<'_>, session: &Bound<'_, PyAny>) -> PyResult<()> {
    let _ = session;
    with_state(py)
}
#[pyfunction]
#[pyo3(signature=(session))]
fn pytest_collection_finish(py: Python<'_>, session: &Bound<'_, PyAny>) -> PyResult<()> {
    let mut guard = state()
        .lock()
        .map_err(|_| PyRuntimeError::new_err("native probe state poisoned"))?;
    let current = guard
        .as_mut()
        .ok_or_else(|| PyRuntimeError::new_err("native probe was not configured"))?;
    if current.node {
        let items = session.getattr("items")?;
        if items.len()? != 1
            || items.get_item(0)?.getattr("nodeid")?.extract::<String>()? != current.nodeid
        {
            return Err(PyRuntimeError::new_err(
                "invariant node must match exactly one collected test",
            ));
        }
    }
    inspect(py, current)
}
#[pyfunction]
#[pyo3(signature=(item,nextitem))]
fn pytest_runtest_teardown(
    py: Python<'_>,
    item: &Bound<'_, PyAny>,
    nextitem: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let _ = (item, nextitem);
    with_state(py)
}
#[pyfunction]
#[pyo3(signature=(session,exitstatus))]
fn pytest_sessionfinish(
    py: Python<'_>,
    session: &Bound<'_, PyAny>,
    exitstatus: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let _ = (session, exitstatus);
    let mut guard = state()
        .lock()
        .map_err(|_| PyRuntimeError::new_err("native probe state poisoned"))?;
    let current = guard
        .as_mut()
        .ok_or_else(|| PyRuntimeError::new_err("native probe was not configured"))?;
    inspect(py, current)?;
    if current.node && current.torch_module.is_none() {
        return Err(PyRuntimeError::new_err(
            "selected test did not observe torch",
        ));
    }
    save(current).map_err(|e| PyRuntimeError::new_err(format!("native probe: {e:#}")))
}
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(pytest_configure, module)?)?;
    module.add_function(wrap_pyfunction!(pytest_sessionstart, module)?)?;
    module.add_function(wrap_pyfunction!(pytest_collection_finish, module)?)?;
    module.add_function(wrap_pyfunction!(pytest_runtest_teardown, module)?)?;
    module.add_function(wrap_pyfunction!(pytest_sessionfinish, module)?)?;
    Ok(())
}
#[cfg(test)]
#[path = "invariant_probe_tests.rs"]
mod tests;
