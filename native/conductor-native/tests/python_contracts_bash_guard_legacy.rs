#![cfg(feature = "python-compat-tests")]
//! Rust-owned contract for the legacy Python Bash guard binding.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{module, Case};

#[test]
fn python_binding_uses_the_native_guard() {
    let _case = Case::new();
    Python::attach(|py| {
        let guard = module(py, "tooling.hooks.claude._bash_guard");
        assert!(guard
            .getattr("check")
            .unwrap()
            .call1(("git reset --hard HEAD",))
            .unwrap()
            .eq("BLOCKED: git reset --hard destroys uncommitted work. Stash or commit first.")
            .unwrap());
        assert!(guard
            .getattr("check")
            .unwrap()
            .call1(("git push --force-with-lease origin main",))
            .unwrap()
            .is_none());
        assert!(guard
            .getattr("check_command")
            .unwrap()
            .call1((vec!["pip", "install", "numpy"],))
            .unwrap()
            .eq("BLOCKED: Use 'uv pip install' instead of raw pip.")
            .unwrap());
    });
}
