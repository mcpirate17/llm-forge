#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the production differential equivalence probe.
//! The Python files in fixtures/equivalence_probe are executable probe inputs,
//! copied verbatim from the retired pytest suite; all assertions live in Rust.
#[path = "python_contracts/equivalence_probe_cases_a.rs"]
mod equivalence_probe_cases_a;
#[path = "python_contracts/equivalence_probe_cases_b.rs"]
mod equivalence_probe_cases_b;
#[path = "python_contracts/equivalence_probe_cases_c.rs"]
mod equivalence_probe_cases_c;
#[path = "python_contracts/equivalence_probe_support.rs"]
mod equivalence_probe_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
