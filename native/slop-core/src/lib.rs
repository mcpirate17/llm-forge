// PyO3's generated `?` conversion in #[pyfunction] bodies trips this Rust 1.93 lint
// even though the handwritten functions perform no redundant conversion (same
// allowance as conductor-native and research-runtime).
#![allow(clippy::useless_conversion)]

//! Native ablation engine for the equivalence probe.
//!
//! Source parsing, rule matching, span rewriting and report construction are native.
//! Transitional CPython adapters require the explicit `python` feature.

pub mod ablation;
pub mod audit_inventory;
pub mod backlog;
#[cfg(feature = "python")]
pub mod consolidation;
pub mod detector_scan;
pub mod engine;
pub mod file_families;
#[cfg(feature = "python")]
pub mod file_profiles;
#[cfg(feature = "python")]
pub mod gate;
pub mod gate_report;
pub mod gate_types;
pub mod hot_loop_scan;
pub mod index;
pub mod ledger;
#[cfg(feature = "python")]
pub mod python_ast;
pub mod repository_scan;
pub mod rules;
pub mod rust_scan;
pub mod source;
pub mod style_scan;
pub mod test_value_scan;
pub mod verbosity_scan;

#[cfg(feature = "python")]
mod python_bindings;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod index_tests;

#[cfg(test)]
mod ledger_tests;

#[cfg(test)]
mod style_scan_tests;

#[cfg(test)]
mod hot_loop_scan_tests;

#[cfg(test)]
mod detector_scan_tests;

#[cfg(test)]
mod rust_scan_tests;

#[cfg(test)]
mod config_scan_tests;

#[cfg(test)]
mod gate_tests;

#[cfg(test)]
mod backlog_tests;
