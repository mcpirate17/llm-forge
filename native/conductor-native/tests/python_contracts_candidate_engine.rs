#![cfg(feature = "python-compat-tests")]
//! Engine identity, layout and installation provenance contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::Path;
use support::{attr_text, module, path, text, AttrPatch, Case};

const LAYOUT: &str = "[tool.conductor]\npackage_root = \"src/conductor\"\n";
const FULL: &str = "5ca892e0db6cf853b7b46be098c63032849e3cfa";
const OTHER: &str = "591f1986b1f1ce2c7f0d4d0d7c4b1c0c3d2e1f00";

fn package_hash(py: Python<'_>, root: &Path) -> (String, Vec<String>) {
    let result = module(py, "conductor.candidate_review.engine")
        .getattr("_package_hash")
        .unwrap()
        .call1((path(py, root),))
        .unwrap();
    let digest = result.get_item(0).unwrap().extract().unwrap();
    let files: std::collections::BTreeMap<String, String> =
        result.get_item(1).unwrap().extract().unwrap();
    let files = files.into_keys().collect();
    (digest, files)
}

#[test]
fn engine_hash_obeys_layout_source_filtering_and_bytes() {
    let case = Case::new();
    let root = case.root();
    Python::attach(|py| {
        assert_eq!(package_hash(py, root), (String::new(), vec![]));
        case.write("pyproject.toml", LAYOUT);
        case.write("conductor/candidate_review/engine.py", "x = 1\n");
        assert_eq!(package_hash(py, root), (String::new(), vec![]));
        case.write("src/conductor/candidate_review/engine.py", "x = 1\n");
        let (digest, files) = package_hash(py, root);
        assert!(!digest.is_empty());
        assert_eq!(files, ["candidate_review/engine.py"]);
        case.write("src/conductor/candidate_review/notes.md", "prose\n");
        case.write("src/conductor/candidate_review/nested/deep.py", "y = 2\n");
        assert_eq!(package_hash(py, root).0, digest);
        case.write("src/conductor/candidate_review/engine.py", "x = 2\n");
        assert_ne!(package_hash(py, root).0, digest);
    });
}

#[test]
fn engine_hash_matches_across_layouts_and_ignores_data_only_package() {
    let case = Case::new();
    let flat = case.mkdir("flat");
    let src = case.mkdir("src-layout");
    case.write("flat/conductor/candidate_review/engine.py", "x = 1\n");
    case.write("src-layout/pyproject.toml", LAYOUT);
    case.write(
        "src-layout/src/conductor/candidate_review/engine.py",
        "x = 1\n",
    );
    Python::attach(|py| {
        let flat_hash = package_hash(py, &flat);
        assert!(!flat_hash.0.is_empty());
        assert_eq!(flat_hash, package_hash(py, &src));
        let empty = case.mkdir("data-only");
        case.write(
            "data-only/conductor/candidate_review/grandfathered_test_nodeids_61343f57.json",
            "[]\n",
        );
        assert_eq!(package_hash(py, &empty), (String::new(), vec![]));
    });
}

#[test]
fn running_checkout_hashes_real_engine_sources() {
    let _case = Case::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/conductor");
    Python::attach(|py| {
        let paths = module(py, "conductor.project_paths");
        let root = paths
            .getattr("package_tree_root")
            .unwrap()
            .call1((path(py, &source),))
            .unwrap();
        let root_string = text(&root);
        let (digest, files) = package_hash(py, Path::new(&root_string));
        assert!(!digest.is_empty());
        assert!(files.contains(&"candidate_review/engine.py".to_owned()));
        assert!(paths
            .getattr("package_path")
            .unwrap()
            .call1((root,))
            .unwrap()
            .call_method0("is_dir")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn pinned_engine_commit_accepts_only_exact_git_lock_fragment() {
    let case = Case::new();
    Python::attach(|py| {
        let pinned = module(py, "conductor.candidate_review.engine")
            .getattr("_pinned_engine_commit")
            .unwrap();
        let get = || pinned.call1((path(py, case.root()),)).unwrap();
        assert!(get().is_none());
        for bad in [
            "",
            "[[package]]\nname = \"requests\"\nversion = \"2.0\"\nsource = { registry = \"x\" }\n",
            "[[package]]\nname = \"conductor-tooling\"\nsource = { editable = \"../llm-forge\" }\n",
            "[[package]]\nname = \"conductor-tooling\"\nsource = { git = \"https://x/y?rev=abc\" }\n",
            "[[package]]\nname = \"conductor-tooling\"\nsource = { git = \"https://x/y#notasha\" }\n",
            "this is not toml [[[",
        ] {
            case.write("uv.lock", bad);
            assert!(get().is_none(), "bad lock {bad:?}");
        }
        case.write("uv.lock", &format!("[[package]]\nname = \"conductor-tooling\"\nversion = \"0.1.0\"\nsource = {{ git = \"https://github.com/mcpirate17/llm-forge?rev=5ca892e#{FULL}\" }}\n"));
        assert_eq!(text(&get()), FULL);
    });
}

fn mock_distribution<'py>(py: Python<'py>, value: Option<&str>) -> Bound<'py, PyAny> {
    let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
    let read_kwargs = PyDict::new(py);
    read_kwargs.set_item("return_value", value).unwrap();
    let read = mock.call((), Some(&read_kwargs)).unwrap();
    let distribution = mock.call0().unwrap();
    distribution.setattr("read_text", read).unwrap();
    distribution
}

#[test]
fn installed_engine_commit_requires_valid_git_provenance() {
    let _case = Case::new();
    Python::attach(|py| {
        let engine = module(py, "conductor.candidate_review.engine");
        let metadata = engine.getattr("importlib_metadata").unwrap();
        let mock = module(py, "unittest.mock").getattr("Mock").unwrap();
        let installed = engine.getattr("_installed_engine_commit").unwrap();
        for (source, expected) in [
            (None, None),
            (
                Some(r#"{"url":"file:///home/x/llm-forge","dir_info":{"editable":true}}"#),
                None,
            ),
            (
                Some(r#"{"url":"https://x","vcs_info":{"vcs":"git"}}"#),
                None,
            ),
            (Some("{not json"), None),
            (
                Some(
                    r#"{"url":"https://github.com/mcpirate17/llm-forge","vcs_info":{"vcs":"git","commit_id":"5ca892e0db6cf853b7b46be098c63032849e3cfa","requested_revision":"5ca892e"}}"#,
                ),
                Some(FULL),
            ),
        ] {
            let kwargs = PyDict::new(py);
            let metadata_distribution = mock_distribution(py, source);
            kwargs
                .set_item("return_value", &metadata_distribution)
                .unwrap();
            let distribution = mock.call((), Some(&kwargs)).unwrap();
            let _patch = AttrPatch::replace(&metadata, "distribution", &distribution);
            let result = installed.call0().unwrap();
            assert_eq!(
                result.extract::<Option<String>>().unwrap().as_deref(),
                expected
            );
            let read_text = metadata_distribution.getattr("read_text").unwrap();
            assert_eq!(
                read_text
                    .getattr("call_count")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                1
            );
            assert_eq!(
                read_text
                    .getattr("call_args")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .extract::<(String,)>()
                    .unwrap(),
                ("direct_url.json".to_owned(),)
            );
        }
        let error = metadata
            .getattr("PackageNotFoundError")
            .unwrap()
            .call1(("conductor-tooling",))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("side_effect", error).unwrap();
        let missing = mock.call((), Some(&kwargs)).unwrap();
        let _patch = AttrPatch::replace(&metadata, "distribution", &missing);
        assert!(installed.call0().unwrap().is_none());
    });
}

fn rules(findings: &Bound<'_, PyAny>) -> Vec<String> {
    findings
        .try_iter()
        .unwrap()
        .map(|item| attr_text(&item.unwrap(), "rule_id"))
        .collect()
}

#[test]
fn engine_findings_prioritize_in_tree_bytes_and_exact_installed_pin() {
    let _case = Case::new();
    Python::attach(|py| {
        let findings = module(py, "conductor.candidate_review.engine")
            .getattr("_engine_findings")
            .unwrap();
        let get =
            |candidate: &str, runtime: &str, installed: Option<&str>, pinned: Option<&str>| {
                findings
                    .call1((candidate, runtime, installed, pinned))
                    .unwrap()
            };
        assert!(rules(&get("h", "h", None, None)).is_empty());
        assert_eq!(
            rules(&get("h", "other", None, None)),
            ["dirty-engine-source"]
        );
        assert_eq!(
            rules(&get("h", "other", Some(FULL), Some(FULL))),
            ["dirty-engine-source"]
        );
        assert!(rules(&get("h", "h", Some(FULL), Some(OTHER))).is_empty());
        assert!(rules(&get("", "runtime", Some(FULL), Some(FULL))).is_empty());
        let mismatch = get("", "runtime", Some(FULL), Some(OTHER));
        assert_eq!(rules(&mismatch), ["engine-pin-mismatch"]);
        let finding = mismatch.get_item(0).unwrap();
        assert_eq!(attr_text(&finding, "severity"), "critical");
        assert!(attr_text(&finding, "message").contains(&FULL[..12]));
        assert!(attr_text(&finding, "message").contains(&OTHER[..12]));
        for (installed, pinned) in [(None, None), (Some(FULL), None), (None, Some(FULL))] {
            let absent = get("", "runtime", installed, pinned);
            assert_eq!(rules(&absent), ["engine-absent-from-candidate"]);
            assert_eq!(
                attr_text(&absent.get_item(0).unwrap(), "severity"),
                "critical"
            );
        }
    });
}
