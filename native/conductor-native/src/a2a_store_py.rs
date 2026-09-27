//! Python handle for the shared durable A2A store.

use crate::a2a_store::{MessageInput, Store};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

fn verify_python_sqlite(py: Python<'_>) -> PyResult<()> {
    let sqlite = PyModule::import(py, "_sqlite3")?;
    let path: String = sqlite.getattr("__file__")?.extract()?;
    let failure = |error| {
        PyRuntimeError::new_err(format!(
            "cannot verify Python SQLite library identity: {error}"
        ))
    };
    // Python has already loaded this trusted extension. Keep the additional
    // library reference alive until its dependency symbol has been compared.
    let library = unsafe { libloading::Library::new(&path) }.map_err(failure)?;
    type Version = unsafe extern "C" fn() -> *const std::ffi::c_char;
    // sqlite3_libversion has this stable C signature. Only compare addresses;
    // no function from an incompatible SQLite runtime is invoked.
    let symbol = unsafe { library.get::<Version>(b"sqlite3_libversion\0") }.map_err(failure)?;
    let python_address = *symbol as *const () as usize;
    let native_address = rusqlite::ffi::sqlite3_libversion as *const () as usize;
    if python_address != native_address {
        return Err(PyRuntimeError::new_err(
            "A2A requires the same SQLite library as Python _sqlite3; rebuild conductor-native \
             without bundled-sqlite and link it to Python's SQLite library",
        ));
    }
    Ok(())
}

fn python_error(error: anyhow::Error) -> PyErr {
    PyValueError::new_err(format!("{error:#}"))
}

fn selected_state(source: Option<&str>) -> PyResult<Option<Value>> {
    source
        .map(|raw| {
            serde_json::from_str(raw).map_err(|error| PyValueError::new_err(error.to_string()))
        })
        .transpose()
}

fn json_output<T: serde::Serialize>(result: anyhow::Result<T>) -> PyResult<String> {
    result
        .and_then(|value| serde_json::to_string(&value).map_err(Into::into))
        .map_err(python_error)
}

#[pyclass]
pub struct A2aSqliteStore {
    inner: Mutex<Store>,
}

impl A2aSqliteStore {
    fn store(&self) -> PyResult<MutexGuard<'_, Store>> {
        self.inner
            .lock()
            .map_err(|_| PyValueError::new_err("A2A store lock is poisoned"))
    }
}

#[pymethods]
impl A2aSqliteStore {
    #[new]
    fn new(py: Python<'_>, root: &str, name: &str) -> PyResult<Self> {
        verify_python_sqlite(py)?;
        let inner = Store::initialize(Path::new(root), name).map_err(python_error)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    #[pyo3(signature = (id, sender, recipient, body, data_json, now, state_json=None))]
    #[allow(clippy::too_many_arguments)] // Mirrors the existing Python A2aStore method.
    fn record_inbound(
        &self,
        id: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        data_json: Option<&str>,
        now: &str,
        state_json: Option<&str>,
    ) -> PyResult<()> {
        let state = selected_state(state_json)?;
        self.store()?
            .record_inbound_with_state(
                MessageInput {
                    id,
                    sender,
                    recipient,
                    body,
                    data_json,
                    now,
                },
                state.as_ref(),
            )
            .map_err(python_error)
    }

    #[pyo3(signature = (id, sender, recipient, body, data_json, now, state_json=None))]
    #[allow(clippy::too_many_arguments)] // Mirrors the existing Python A2aStore method.
    fn record_outbound(
        &self,
        id: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        data_json: Option<&str>,
        now: &str,
        state_json: Option<&str>,
    ) -> PyResult<()> {
        let state = selected_state(state_json)?;
        self.store()?
            .record_outbound(
                MessageInput {
                    id,
                    sender,
                    recipient,
                    body,
                    data_json,
                    now,
                },
                state.as_ref(),
            )
            .map_err(python_error)
    }

    fn mark_outbound(
        &self,
        id: &str,
        status: &str,
        reason: Option<&str>,
        received_at: Option<&str>,
        now: &str,
    ) -> PyResult<()> {
        self.store()?
            .mark_outbound(id, status, reason, now, received_at)
            .map_err(python_error)
    }

    fn mark_read(&self, id: &str, now: &str) -> PyResult<String> {
        json_output(self.store()?.mark_read(id, now))
    }

    fn queued_rows(
        &self,
        recipient: Option<&str>,
        excluded: Vec<String>,
        limit: Option<i64>,
    ) -> PyResult<String> {
        json_output(self.store()?.queued_rows(recipient, &excluded, limit))
    }

    fn inbound_rows(&self, unread_only: bool, limit: i64) -> PyResult<String> {
        json_output(self.store()?.inbound_rows(unread_only, limit))
    }

    fn preview_rows(
        &self,
        unread_only: bool,
        unpresented_only: bool,
        limit: i64,
        preview_chars: i64,
    ) -> PyResult<String> {
        json_output(
            self.store()?
                .preview_rows(unread_only, unpresented_only, limit, preview_chars),
        )
    }

    fn fetch_row(&self, id: &str, direction: &str) -> PyResult<String> {
        json_output(self.store()?.fetch_row(id, direction))
    }

    fn mark_presented(&self, ids: Vec<String>, now: &str) -> PyResult<usize> {
        self.store()?
            .mark_presented(&ids, now)
            .map_err(python_error)
    }

    fn resolve(&self, id: &str, now: &str) -> PyResult<String> {
        json_output(self.store()?.resolve(id, now))
    }

    fn set_hold(&self, id: &str, reason: Option<&str>) -> PyResult<()> {
        self.store()?.set_hold(id, reason).map_err(python_error)
    }

    fn counts(&self) -> PyResult<String> {
        json_output(self.store()?.counts())
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<A2aSqliteStore>()?;
    Ok(())
}
