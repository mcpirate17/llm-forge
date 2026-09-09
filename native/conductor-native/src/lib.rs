//! `conductor_native`: the Rust core of the agent tooling under `conductor/`.
//!
//! `conductor/_native.py` is the only importer. Everything here used to live in
//! research-runtime; it moved so the tooling can be built, tested and shipped without
//! the project's research crate (tooling boundary step 2a).

mod a2a_compaction;
mod a2a_retention;
mod branch_policy;
mod candidate_structure;
mod context_telemetry;
mod dead_tests;
mod duplicate_bodies;
mod fleet_status;
mod git_source;
mod guardrail_ast;
mod hook_installer;
mod hook_merge;
mod kb_retrieve;
mod memory_chunking;
mod memory_index;
mod mutation_coverage;
mod mutation_evidence;
mod mutation_manifest;
mod mutation_receipt;
mod mutation_value;
mod native_reuse;
mod receipt_auth;
mod tooling_boundary;

use pyo3::prelude::*;

#[pymodule]
fn conductor_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    a2a_compaction::register(module)?;
    a2a_retention::register(module)?;
    branch_policy::register(module)?;
    candidate_structure::register(module)?;
    context_telemetry::register(module)?;
    dead_tests::register(module)?;
    duplicate_bodies::register(module)?;
    fleet_status::register(module)?;
    guardrail_ast::register(module)?;
    hook_installer::register(module)?;
    hook_merge::register(module)?;
    git_source::register(module)?;
    mutation_coverage::register(module)?;
    mutation_evidence::register(module)?;
    kb_retrieve::register(module)?;
    memory_chunking::register(module)?;
    memory_index::register(module)?;
    mutation_value::register(module)?;
    native_reuse::register(module)?;
    receipt_auth::register(module)?;
    tooling_boundary::register(module)?;
    Ok(())
}
