//! Synthetic repository inputs for Rust-owned generator contracts.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::support::{module, path};

pub const CRATE: &str = "\n[package]\nname = \"widget-core\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"not-the-package\"\n\n[dependencies]\nname = \"also-not-the-package\"\n";
pub const RUST_UNIT: &str =
    "\npub fn keep() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n";

pub fn generator<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_campaign_generate").into_any()
}

pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn py_to_json(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let text: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

pub fn tree(root: &Path, files: &[(&str, &str)]) {
    for (relative, contents) in files {
        let file = root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, contents).unwrap();
    }
}

pub fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

pub fn git(root: &Path, args: &[&str]) {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&result.stderr)
    );
}

pub fn git_repo(root: &Path, files: &[(&str, &str)]) {
    tree(root, files);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "scope@example.invalid"]);
    git(root, &["config", "user.name", "scope probe"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
}

pub fn plan<'py>(
    py: Python<'py>,
    language: &str,
    root: &Path,
    extra: &Value,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root))?;
    if let Some(day) = extra.get("day") {
        kw.set_item("day", json_to_py(py, day))?;
    }
    if let Some(sources) = extra.get("only_sources") {
        kw.set_item("only_sources", json_to_py(py, sources))?;
    }
    if let Some(tests) = extra.get("extra_tests") {
        kw.set_item("extra_tests", json_to_py(py, tests))?;
    }
    if let Some(covered) = extra.get("include_covered") {
        kw.set_item("include_covered", json_to_py(py, covered))?;
    }
    generator(py).getattr("plan")?.call((language,), Some(&kw))
}

pub fn plan_json(py: Python<'_>, language: &str, root: &Path, extra: &Value) -> Value {
    py_to_json(&plan(py, language, root, extra).unwrap())
}

pub fn first(planned: &Value) -> Value {
    planned["manifests"]
        .as_array()
        .unwrap()
        .first()
        .expect("at least one manifest")
        .clone()
}

pub fn write(
    py: Python<'_>,
    root: &Path,
    manifests: &[Value],
    force: bool,
) -> PyResult<Vec<String>> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root))?;
    if force {
        kw.set_item("force", true)?;
    }
    generator(py)
        .getattr("write")?
        .call((json_to_py(py, &json!(manifests)),), Some(&kw))?
        .extract()
}

pub fn refresh(
    py: Python<'_>,
    name: &str,
    id: &str,
    root: &Path,
    sources: Option<&[&str]>,
) -> PyResult<String> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root))?;
    if let Some(sources) = sources {
        kw.set_item("sources", sources)?;
    }
    generator(py)
        .getattr(name)?
        .call((id,), Some(&kw))?
        .extract()
}

pub fn manifest_file(root: &Path, written: &[String]) -> PathBuf {
    root.join(written.first().unwrap())
}

pub fn load_json(file: &Path) -> Value {
    serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
}

pub fn assert_names_own_tests(manifest: &Value) {
    let source = manifest["generator"]["source"][0].as_str().unwrap();
    let stem = source.rsplit('/').next().unwrap().trim_end_matches(".py");
    let argv = manifest["test_argv"].as_array().unwrap();
    assert_eq!(
        &argv[..5],
        &json!(["python", "-m", "pytest", "-q", "--rootdir=."])
            .as_array()
            .unwrap()[..]
    );
    let tests = &argv[5..];
    assert!(!tests.is_empty());
    assert!(tests
        .iter()
        .any(|test| test.as_str().unwrap().contains(stem)));
    let pins = manifest["test_sha256"].as_object().unwrap();
    let mut tests_sorted: Vec<_> = tests.iter().map(|test| test.as_str().unwrap()).collect();
    tests_sorted.sort();
    let mut pins_sorted: Vec<_> = pins.keys().map(String::as_str).collect();
    pins_sorted.sort();
    assert_eq!(tests_sorted, pins_sorted);
    assert_eq!(
        manifest["generator"]
            .get("jobs")
            .and_then(Value::as_i64)
            .unwrap_or(1),
        1
    );
}

pub fn claim(py: Python<'_>, root: &Path, owner: &str, paths: &[&str]) {
    let kw = PyDict::new(py);
    kw.set_item("owner", owner).unwrap();
    kw.set_item("paths", paths).unwrap();
    kw.set_item("justification", "synthetic Rust-owned scope fixture")
        .unwrap();
    module(py, "conductor.candidate_review.ownership")
        .getattr("create_claim")
        .unwrap()
        .call((path(py, root),), Some(&kw))
        .unwrap();
}
