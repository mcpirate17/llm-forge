//! Rust-owned synthetic checkout fixtures for the native freshness contract.

use crate::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use std::fs;
use std::path::{Path, PathBuf};

pub const PYPROJECT: &str = "[project]\nname = \"demo-native\"\nversion = \"1.2.3\"\n\n[tool.maturin]\nmodule-name = \"demo_native\"\n";
pub const NO_MODULE: &str = "[project]\nname = \"demo-binary\"\nversion = \"0.2.0\"\n";

pub fn freshness(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "tooling.hooks.dispatch.native_freshness")
}

pub fn make_crate(root: &Path, version: &str, directory: &str) -> PathBuf {
    let crate_dir = root.join("tooling/native").join(directory);
    fs::create_dir_all(crate_dir.join("src")).unwrap();
    fs::write(
        crate_dir.join("pyproject.toml"),
        PYPROJECT.replace("1.2.3", version),
    )
    .unwrap();
    fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"demo-native\"\n",
    )
    .unwrap();
    fs::write(crate_dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    fs::write(crate_dir.join("README.md"), "not a source\n").unwrap();
    crate_dir
}

pub fn standard_crate(root: &Path) -> PathBuf {
    make_crate(root, "1.2.3", "demo-native")
}

pub fn install(root: &Path, version: &str, url: Option<&str>) -> PathBuf {
    let info = root.join(format!(
        ".venv/lib/python3.12/site-packages/demo_native-{version}.dist-info"
    ));
    fs::create_dir_all(&info).unwrap();
    fs::write(info.join("METADATA"), "Name: demo-native\n").unwrap();
    if let Some(url) = url {
        fs::write(
            info.join("direct_url.json"),
            format!("{{\"url\":{}}}", serde_json::to_string(url).unwrap()),
        )
        .unwrap();
    }
    info
}

pub fn crates<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    freshness(py)
        .getattr("crates")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
}

pub fn checkout(case: &Case, py: Python<'_>) -> PathBuf {
    let crate_dir = standard_crate(case.root());
    install(
        case.root(),
        "1.2.3",
        Some(&format!("file://{}", crate_dir.display())),
    );
    let first = crates(py, case.root()).get_item(0).unwrap();
    freshness(py)
        .getattr("write_stamp")
        .unwrap()
        .call1((path(py, case.root()), first))
        .unwrap();
    crate_dir
}

pub fn lines(py: Python<'_>, root: &Path) -> Vec<String> {
    freshness(py)
        .getattr("findings")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|row| {
            row.unwrap()
                .call_method0("line")
                .unwrap()
                .extract()
                .unwrap()
        })
        .collect()
}
