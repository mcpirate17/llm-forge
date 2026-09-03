//! `conductor_native`: the Rust core of the agent tooling under `conductor/`.
//!
//! `conductor/_native.py` is the only importer. Everything here used to live in
//! research-runtime; it moved so the tooling can be built, tested and shipped without
//! the project's research crate (tooling boundary step 2a).

mod candidate_structure;
mod dead_tests;
mod duplicate_bodies;
mod guardrail_ast;
mod mutation_coverage;
mod mutation_evidence;
mod mutation_manifest;
mod mutation_receipt;
mod native_reuse;
mod tooling_boundary;

use pyo3::prelude::*;

#[pymodule]
fn conductor_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    candidate_structure::register(module)?;
    dead_tests::register(module)?;
    duplicate_bodies::register(module)?;
    guardrail_ast::register(module)?;
    mutation_coverage::register(module)?;
    mutation_evidence::register(module)?;
    native_reuse::register(module)?;
    tooling_boundary::register(module)?;
    Ok(())
}
