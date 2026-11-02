#![cfg(feature = "python-compat-tests")]

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{module, path, text, Case};

#[test]
fn vault_root_derives_from_home_unless_overridden() {
    let mut case = Case::new();
    case.remove_env("CODEX_VAULT_ROOT");
    Python::attach(|py| {
        let bundle = module(py, "conductor.notebooklm_bundle");
        assert_eq!(
            text(&bundle.getattr("VAULT_ROOT_ENV").unwrap()),
            "CODEX_VAULT_ROOT"
        );
        let expected = module(py, "pathlib")
            .getattr("Path")
            .unwrap()
            .call_method0("home")
            .unwrap()
            .call_method1("joinpath", ("Documents", "CodexVault"))
            .unwrap();
        assert!(bundle
            .call_method0("_vault_root")
            .unwrap()
            .eq(expected)
            .unwrap());
        let vault = case.root().join("vault");
        case.set_env("CODEX_VAULT_ROOT", vault.to_str().unwrap());
        assert!(bundle
            .call_method0("_vault_root")
            .unwrap()
            .eq(path(py, &vault))
            .unwrap());
    });
}
