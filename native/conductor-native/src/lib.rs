//! `conductor_native`: the Rust core of the agent tooling under `conductor/`.
//!
//! `conductor/_native.py` is the only importer. Everything here used to live in
//! research-runtime; it moved so the tooling can be built, tested and shipped without
//! the project's research crate (tooling boundary step 2a).
//!
//! Default builds include the Python extension API. Native executables can disable
//! default features to use the shared planning, receipt and shell parsing cores
//! without compiling Python bindings or linking an interpreter.

mod a2a_compaction;
#[cfg(feature = "python")]
mod a2a_retention;
#[cfg(feature = "python")]
mod branch_policy;
#[cfg(feature = "python")]
mod candidate_structure;
#[cfg(feature = "python")]
mod context_telemetry;
#[cfg(feature = "python")]
mod dead_tests;
#[cfg(feature = "python")]
mod duplicate_bodies;
#[cfg(feature = "python")]
mod fleet_status;
#[cfg(feature = "python")]
mod git_source;
#[cfg(feature = "python")]
mod guardrail_ast;
#[cfg(feature = "python")]
mod guardrail_duplicates;
pub mod hook_installer;
#[cfg(feature = "python")]
mod hook_merge;
#[cfg(feature = "python")]
mod kb_retrieve;
#[cfg(feature = "python")]
mod memory_chunking;
#[cfg(feature = "python")]
mod memory_index;
#[cfg(feature = "python")]
mod memory_index_sidecar;
#[cfg(feature = "python")]
mod mutation_coverage;
#[cfg(feature = "python")]
mod mutation_evidence;
#[cfg(feature = "python")]
mod mutation_manifest;
pub mod mutation_plan;
#[cfg(feature = "python")]
mod mutation_receipt;
#[cfg(feature = "python")]
mod mutation_value;
#[cfg(feature = "python")]
mod native_reuse;
#[cfg(feature = "python")]
mod project_context;
#[cfg(feature = "python")]
mod receipt_auth;
pub mod receipt_slim;
mod text_normalization;
#[cfg(feature = "python")]
mod tooling_boundary;

pub use a2a_compaction::{
    compact_message_value as compact_a2a_message, compact_threads_value as compact_a2a_threads,
    validate_coordination_v2_value,
};
pub use text_normalization::normalized_text as normalize_a2a_text;

#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
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
    guardrail_duplicates::register(module)?;
    hook_installer::register(module)?;
    hook_merge::register(module)?;
    git_source::register(module)?;
    mutation_coverage::register(module)?;
    mutation_evidence::register(module)?;
    mutation_plan::register(module)?;
    kb_retrieve::register(module)?;
    memory_chunking::register(module)?;
    memory_index::register(module)?;
    memory_index_sidecar::register(module)?;
    mutation_value::register(module)?;
    native_reuse::register(module)?;
    project_context::register(module)?;
    receipt_auth::register(module)?;
    receipt_slim::register(module)?;
    tooling_boundary::register(module)?;
    Ok(())
}
