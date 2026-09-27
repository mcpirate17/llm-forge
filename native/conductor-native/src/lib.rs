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
pub mod a2a_retention;
pub mod a2a_store;
#[cfg(feature = "python")]
mod a2a_store_py;
#[cfg(feature = "python")]
mod branch_policy;
#[cfg(feature = "python")]
mod candidate_benchmark;
pub mod candidate_checks;
pub mod candidate_policy;
#[cfg(feature = "source-analysis")]
pub mod candidate_structure;
#[cfg(feature = "source-analysis")]
pub mod candidate_verification;
#[cfg(feature = "python")]
mod context_telemetry;
pub mod context_telemetry_aggregate;
#[cfg(all(feature = "source-analysis", any(feature = "python", test)))]
mod dead_tests;
#[cfg(feature = "python")]
mod duplicate_bodies;
#[cfg(feature = "python")]
mod fleet_status;
pub mod forge_binary;
#[cfg(feature = "python")]
mod git_source;
pub mod graph_context;
#[cfg(feature = "source-analysis")]
pub mod graph_index;
#[cfg(feature = "python")]
mod guardrail_ast;
#[cfg(feature = "python")]
mod guardrail_duplicates;
pub mod hook_installer;
#[cfg(feature = "python")]
mod hook_merge;
pub mod kb_retrieve;
pub mod memory_chunking;
#[cfg(feature = "python")]
mod memory_index;
#[cfg(feature = "python")]
mod memory_index_sidecar;
pub mod mutation_coverage;
#[cfg(feature = "python")]
mod mutation_evidence;
#[cfg(all(feature = "source-analysis", any(feature = "python", test)))]
mod mutation_manifest;
pub mod mutation_plan;
#[cfg(feature = "python")]
mod mutation_receipt;
#[cfg(feature = "python")]
mod mutation_value;
pub mod mutation_value_inputs;
#[cfg(feature = "python")]
mod native_reuse;
pub mod project_context;
pub mod project_paths;
#[cfg(all(feature = "source-analysis", any(feature = "python", test)))]
mod receipt_auth;
pub mod receipt_slim;
pub mod reuse_consolidation;
pub mod test_contracts;
mod text_normalization;
#[cfg(feature = "python")]
mod tooling_boundary;
pub mod workspace_runtime_matrix;

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
    candidate_benchmark::register(module)?;
    candidate_checks::register(module)?;
    candidate_structure::register(module)?;
    candidate_verification::register(module)?;
    context_telemetry::register(module)?;
    dead_tests::register(module)?;
    duplicate_bodies::register(module)?;
    fleet_status::register(module)?;
    forge_binary::register(module)?;
    graph_context::register(module)?;
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
    project_paths::register_py(module)?;
    candidate_policy::register(module)?;
    a2a_store_py::register(module)?;
    receipt_auth::register(module)?;
    receipt_slim::register(module)?;
    reuse_consolidation::register(module)?;
    test_contracts::register(module)?;
    tooling_boundary::register(module)?;
    workspace_runtime_matrix::register(module)?;
    Ok(())
}
