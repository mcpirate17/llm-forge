//! `conductor_native`: the Rust core of the agent tooling under `conductor/`.
//!
//! Forge consumes the native interfaces directly. Transitional adapters also use
//! this library while the remaining interpreted entry points are migrated.
//!
//! Default builds expose native source analysis, vector retrieval and shared
//! planning without compiling bindings or linking an interpreter. Transitional
//! Python adapters require the explicit `python` feature until their callers are
//! migrated; the standalone Forge binary selects bundled SQLite separately.

mod a2a_compaction;
pub mod a2a_retention;
pub mod a2a_store;
pub mod bounded_io;
#[cfg(feature = "python")]
mod branch_policy;
#[cfg(feature = "python")]
mod candidate_benchmark;
pub mod candidate_checks;
pub mod candidate_policy;
#[cfg(feature = "source-analysis")]
pub mod candidate_structure;
pub mod candidate_unicode;
#[cfg(feature = "source-analysis")]
pub mod candidate_verification;
pub mod context_projection;
pub mod context_telemetry_aggregate;
pub mod context_telemetry_usage;
#[cfg(feature = "source-analysis")]
pub mod dead_code;
#[cfg(all(feature = "source-analysis", any(feature = "python", test)))]
mod dead_tests;
#[cfg(feature = "python")]
mod duplicate_bodies;
#[cfg(feature = "python")]
mod equivalence_targets;
pub mod fleet_status;
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
pub mod hook_response;
pub mod kb_retrieve;
pub mod memory_chunking;
#[cfg(feature = "vector-search")]
pub mod memory_index;
#[cfg(feature = "vector-search")]
pub mod memory_index_sidecar;
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
#[cfg(target_os = "linux")]
pub mod native_id;
#[cfg(feature = "python")]
mod native_reuse;
pub mod performance_receipt;
pub mod project_config;
pub mod project_context;
pub mod project_paths;
#[cfg(all(feature = "source-analysis", any(feature = "python", test)))]
mod receipt_auth;
pub mod receipt_slim;
pub mod reuse_consolidation;
pub mod sha256_hex;
pub mod state_paths;
#[cfg(feature = "source-analysis")]
pub mod syntax;
pub mod test_contracts;
#[cfg(feature = "python")]
mod test_record;
#[cfg(feature = "vector-search")]
mod top_k;
#[cfg(feature = "vector-search")]
pub mod vector_index;
// Test selection reads the record, so these two build without Python too; the rest of
// each serves the Python recorder.
#[cfg_attr(not(feature = "python"), allow(dead_code))]
mod test_record_config;
#[cfg(feature = "python")]
mod test_record_digest;
#[cfg(feature = "python")]
mod test_record_finish;
#[cfg(feature = "python")]
mod test_record_installed;
#[cfg(feature = "python")]
mod test_record_paths;
#[cfg(feature = "python")]
mod test_record_recorder;
#[cfg(feature = "python")]
mod test_record_reuse;
#[cfg(feature = "python")]
mod test_record_state;
#[cfg_attr(not(feature = "python"), allow(dead_code))]
mod test_record_store;
#[cfg(feature = "python")]
mod test_record_untested;
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
    branch_policy::register(module)?;
    candidate_benchmark::register(module)?;
    candidate_checks::register(module)?;
    candidate_structure::register(module)?;
    candidate_verification::register(module)?;
    dead_tests::register(module)?;
    duplicate_bodies::register(module)?;
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
    mutation_value::register(module)?;
    native_reuse::register(module)?;
    project_paths::register_py(module)?;
    candidate_policy::register(module)?;
    receipt_auth::register(module)?;
    receipt_slim::register(module)?;
    reuse_consolidation::register(module)?;
    test_contracts::register(module)?;
    test_record::register(module)?;
    tooling_boundary::register(module)?;
    workspace_runtime_matrix::register(module)?;
    Ok(())
}
