//! `TestRecorder`: the handle `forge_test_record` registers as the
//! `sys.monitoring` callbacks and the audit hook.
//!
//! Callbacks never raise into the code being tested. An internal error marks the
//! session failed, and `finish` then refuses to store anything. The state lock is
//! never held while Python code can run, so a callback that finds it held has
//! re-entered: that also fails the session rather than dropping a hit.

use crate::test_record_finish::{self as finish, Environment, Session, Snapshot};
use crate::test_record_installed::Installed;
use crate::test_record_paths::Places;
use crate::test_record_reuse as reuse;
use crate::test_record_state::{Access, Kind, State};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyModule, PyString, PyTuple};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};

/// Audited events after which a test's result depends on something the record
/// cannot see: another process, the network, or a loaded C library.
const OPAQUE_EVENTS: [&str; 10] = [
    "subprocess.Popen",
    "os.system",
    "os.exec",
    "os.posix_spawn",
    "os.spawn",
    "os.fork",
    "os.forkpty",
    "socket.connect",
    "socket.sendto",
    "socket.sendmsg",
];

/// Variables every record is keyed on: a GPU-visible run is not a CPU run.
const ALWAYS_KEYED: [&str; 1] = ["CUDA_VISIBLE_DEVICES"];

/// `str`, `bytes` or `os.PathLike`; `None` for a file descriptor or anything else.
fn path_arg(value: &Bound<'_, PyAny>) -> Option<PathBuf> {
    if let Ok(text) = value.cast::<PyString>() {
        return text.to_str().ok().map(PathBuf::from);
    }
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Some(PathBuf::from(OsStr::from_bytes(bytes.as_bytes())));
    }
    if value.hasattr(intern!(value.py(), "__fspath__")).ok()? {
        let fspath = value.call_method0(intern!(value.py(), "__fspath__")).ok()?;
        if fspath.cast::<PyString>().is_ok() || fspath.cast::<PyBytes>().is_ok() {
            return path_arg(&fspath);
        }
    }
    None
}

fn reads(mode: &Bound<'_, PyAny>, flags: &Bound<'_, PyAny>) -> bool {
    if let Ok(mode) = mode.extract::<&str>() {
        return mode.contains('r') || mode.contains('+');
    }
    flags
        .extract::<i64>()
        .is_ok_and(|flags| flags & i64::from(libc::O_ACCMODE) != i64::from(libc::O_WRONLY))
}

fn sqlite_path(database: &Bound<'_, PyAny>) -> Option<PathBuf> {
    let path = path_arg(database)?;
    let text = path.to_str()?;
    if text.is_empty() || text == ":memory:" {
        return None;
    }
    match text.strip_prefix("file:") {
        Some(uri) => {
            let file = uri.split(['?', '#']).next()?;
            (!file.is_empty() && !uri.contains("mode=memory")).then(|| PathBuf::from(file))
        }
        None => Some(path),
    }
}

fn access(event: &str, args: &Bound<'_, PyTuple>) -> Option<Access> {
    let arg = |index| args.get_item(index).ok();
    match event {
        "open" => {
            let (path, mode, flags) = (arg(0)?, arg(1)?, arg(2)?);
            reads(&mode, &flags)
                .then(|| path_arg(&path))
                .flatten()
                .map(Access::Read)
        }
        "os.listdir" | "os.scandir" => match arg(0) {
            Some(path) if !path.is_none() => path_arg(&path).map(Access::List),
            _ => Some(Access::List(PathBuf::from("."))),
        },
        "sqlite3.connect" => sqlite_path(&arg(0)?).map(Access::Read),
        "ctypes.dlopen" => {
            let name = path_arg(&arg(0)?)?;
            name.to_str()?.contains('/').then_some(Access::Read(name))
        }
        _ => OPAQUE_EVENTS
            .iter()
            .find(|opaque| **opaque == event)
            .map(|opaque| Access::Opaque(opaque)),
    }
}

fn filename(code: &Bound<'_, PyAny>) -> Option<String> {
    let name = code.getattr(intern!(code.py(), "co_filename")).ok()?;
    name.cast::<PyString>()
        .ok()?
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// `(name, __file__)` of loaded modules, read from module dicts so a module-level
/// `__getattr__` never runs.
fn loaded_modules(py: Python<'_>) -> PyResult<Vec<(String, String)>> {
    let modules = PyModule::import(py, "sys")?.getattr("modules")?;
    let modules = modules.cast::<PyDict>()?.copy()?;
    let mut out = Vec::new();
    for (name, module) in modules.iter() {
        let Ok(module) = module.cast::<PyModule>() else {
            continue;
        };
        let Some(file) = module.dict().get_item("__file__")? else {
            continue;
        };
        if let (Ok(name), Ok(file)) = (name.extract::<String>(), file.extract::<String>()) {
            out.push((name, file));
        }
    }
    Ok(out)
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

#[pyclass(frozen)]
pub(crate) struct TestRecorder {
    state: Mutex<Option<State>>,
    reentered: AtomicBool,
    closed: AtomicBool,
    tool: u8,
    lines: bool,
    disable: Py<PyAny>,
    line_event: Py<PyAny>,
    set_local_events: Py<PyAny>,
    restart_events: Py<PyAny>,
    environment: Environment,
    started: f64,
    argv: String,
}

impl TestRecorder {
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        match self.state.try_lock() {
            Ok(mut guard) => guard.as_mut().map(f),
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
                self.reentered.store(true, Ordering::SeqCst);
                None
            }
        }
    }

    fn open(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }
}

#[pymethods]
impl TestRecorder {
    /// `places` is `(environment prefixes, scratch directories, home, site directories)`.
    #[new]
    fn new(
        py: Python<'_>,
        root: &str,
        tool: u8,
        lines: bool,
        environment: Vec<String>,
        places: (Vec<String>, Vec<String>, Option<String>, Vec<String>),
        instrument: Vec<String>,
    ) -> PyResult<Self> {
        let (prefixes, scratch, home, site_dirs) = places;
        let sys = PyModule::import(py, "sys")?;
        let monitoring = sys.getattr("monitoring")?;
        let version: String = sys.getattr("version")?.extract()?;
        let cache_tag: Option<String> = sys
            .getattr("implementation")?
            .getattr("cache_tag")?
            .extract()?;
        let argv: Vec<String> = sys.getattr("argv")?.extract()?;
        let vars: BTreeMap<String, Option<String>> = ALWAYS_KEYED
            .iter()
            .map(|name| (*name).to_owned())
            .chain(environment)
            .map(|name| {
                let value =
                    std::env::var_os(&name).map(|value| value.to_string_lossy().into_owned());
                (name, value)
            })
            .collect();
        let installed_error = |error: std::io::Error| {
            PyValueError::new_err(format!("installed distributions: {error}"))
        };
        let names = Installed::names(&site_dirs).map_err(installed_error)?;
        let installed = Installed::new(&site_dirs).map_err(installed_error)?;
        let places =
            Places::new(root, &prefixes, &scratch, home.as_deref()).with_installed(installed);
        let mut state = State::new(places);
        state.exclude(&instrument);
        for (_, file) in loaded_modules(py)? {
            state.preloaded(&file);
        }
        Ok(Self {
            state: Mutex::new(Some(state)),
            reentered: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            tool,
            lines,
            disable: monitoring.getattr("DISABLE")?.unbind(),
            line_event: monitoring.getattr("events")?.getattr("LINE")?.unbind(),
            set_local_events: monitoring.getattr("set_local_events")?.unbind(),
            restart_events: monitoring.getattr("restart_events")?.unbind(),
            environment: Environment {
                python: format!("{version} {}", cache_tag.unwrap_or_default()),
                vars,
                site_dirs,
                installed: names,
            },
            started: unix_now(),
            argv: serde_json::to_string(&argv).expect("strings serialize"),
        })
    }

    /// `PY_START`: attribute the code object's file, and turn on line events for
    /// repository source.
    fn on_start(
        &self,
        py: Python<'_>,
        code: &Bound<'_, PyAny>,
        _offset: &Bound<'_, PyAny>,
    ) -> Py<PyAny> {
        if self.open() {
            if let Some(name) = filename(code) {
                let source = self.with_state(|state| state.hit(&name)) == Some(true);
                if source && self.lines {
                    let enable = (self.tool, code, self.line_event.bind(py));
                    if let Err(error) = self.set_local_events.call1(py, enable) {
                        self.fail(format!("set_local_events on {name}: {error}"));
                    }
                }
            }
        }
        self.disable.clone_ref(py)
    }

    fn on_line(&self, py: Python<'_>, code: &Bound<'_, PyAny>, line: usize) -> Py<PyAny> {
        if self.open() {
            if let Some(name) = filename(code) {
                self.with_state(|state| state.line(&name, line));
            }
        }
        self.disable.clone_ref(py)
    }

    fn on_audit(&self, event: &str, args: &Bound<'_, PyTuple>) {
        if !self.open() {
            return;
        }
        // Argument conversion can run `__fspath__`, so it happens before locking.
        if let Some(access) = access(event, args) {
            self.with_state(|state| state.access(access));
        }
    }

    /// `restart` re-arms every disabled event so the new window sees code that
    /// already ran; function-scoped fixtures run inside their test's window and skip it.
    fn push(&self, py: Python<'_>, kind: &str, key: String, restart: bool) {
        let Some(kind) = Kind::parse(kind) else {
            return self.fail(format!("unknown window kind {kind:?}"));
        };
        self.with_state(|state| state.push(kind, key));
        if restart {
            if let Err(error) = self.restart_events.call0(py) {
                self.fail(format!("restart_events: {error}"));
            }
        }
    }

    fn pop(&self, kind: &str, key: &str) {
        match Kind::parse(kind) {
            Some(kind) => {
                self.with_state(|state| state.pop(kind, key));
            }
            None => self.fail(format!("unknown window kind {kind:?}")),
        }
    }

    fn report(&self, nodeid: &str, when: &str, outcome: &str, wasxfail: bool) {
        self.with_state(|state| state.report(nodeid, when, outcome, wasxfail));
    }

    fn end_test(
        &self,
        nodeid: &str,
        path: &str,
        module: Option<String>,
        fixtures: Vec<String>,
        seconds: f64,
    ) {
        self.with_state(|state| state.end_test(nodeid, Path::new(path), module, fixtures, seconds));
    }

    /// Mark the session unrecordable; `finish` reports `reason`.
    fn fail(&self, reason: String) {
        self.with_state(|state| state.fail(reason));
    }

    /// Stop attributing events. The audit hook cannot be removed, so it stays
    /// registered and returns at once.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// For each collected `(nodeid, path)`, the `(session, seconds)` of the recorded
    /// pass it can be skipped on, or `None`.
    fn reusable(
        &self,
        py: Python<'_>,
        store: &str,
        tests: Vec<(String, String)>,
    ) -> PyResult<Vec<Option<(i64, f64)>>> {
        let places = self
            .state
            .lock()
            .map_err(|_| PyRuntimeError::new_err("test recorder state is poisoned"))?
            .as_ref()
            .map(|state| state.places.clone())
            .ok_or_else(|| PyRuntimeError::new_err("the session already finished"))?;
        let environment = self.environment.digest();
        let found = py
            .detach(|| reuse::reusable(Path::new(store), &environment, &places, &tests))
            .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))?;
        Ok(found
            .into_iter()
            .map(|hit| hit.map(|hit| (hit.session, hit.seconds)))
            .collect())
    }

    /// Write the session to `store` and return a one-line summary.
    #[pyo3(signature = (store, inipath=None))]
    fn finish(&self, py: Python<'_>, store: &str, inipath: Option<String>) -> PyResult<String> {
        self.close();
        if self.reentered.load(Ordering::SeqCst) {
            return Err(PyRuntimeError::new_err(
                "a monitoring callback re-entered the test recorder; nothing was recorded",
            ));
        }
        let snapshot = Snapshot {
            modules: loaded_modules(py)?,
            inipath,
        };
        let state = self
            .state
            .lock()
            .map_err(|_| PyRuntimeError::new_err("test recorder state is poisoned"))?
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("finish was already called"))?;
        let session = Session {
            store: Path::new(store),
            started: self.started,
            argv: &self.argv,
            environment: &self.environment,
            snapshot: &snapshot,
        };
        py.detach(|| finish::finish(state, &session))
            .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
    }
}
