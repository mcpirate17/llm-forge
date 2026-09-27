#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped CPU embedding broker.

#[path = "python_contracts/cpu_embed_cases_a.rs"]
mod cpu_embed_cases_a;
#[path = "python_contracts/cpu_embed_cases_b.rs"]
mod cpu_embed_cases_b;
#[path = "python_contracts/cpu_embed_cases_c.rs"]
mod cpu_embed_cases_c;
#[path = "python_contracts/cpu_embed_support.rs"]
mod cpu_embed_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
