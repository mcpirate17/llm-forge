//! The test record: which repository files, data and environment each pytest run
//! depended on, and its outcome.
//!
//! `forge_test_record` (a pytest plugin) drives the `test_record_recorder`
//! `TestRecorder` from `sys.monitoring` and an audit hook; the `test_record_*`
//! modules are its parts. A test's dependencies are the files whose code
//! it executed, the modules those files import, the files and directory listings it
//! read, its conftests, the fixtures it used and its module's collection. Repository
//! files are recorded by git blob oid, host files by `stat` identity. A run that
//! started a process, opened a socket or loaded a C library by path is marked opaque.
//!
//! The land reuses a recorded pass only when every dependency still has its recorded
//! value; the nightly full sweep re-runs everything and catches what the record
//! cannot see (native code reading files, import-time state of modules a test
//! neither executed nor imported).

use pyo3::prelude::*;

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<crate::test_record_recorder::TestRecorder>()?;
    module.add_function(wrap_pyfunction!(
        crate::test_record_config::test_record_config_native,
        module
    )?)?;
    Ok(())
}
