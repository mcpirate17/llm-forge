#![cfg(feature = "python-compat-tests")]
//! Rust-owned campaign-generation contracts; no mutation engine is executed.

#[path = "python_contracts/mutation_campaign_generate_support.rs"]
mod generate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use generate_support::{
    assert_names_own_tests, claim, first, generator, git, git_repo, load_json, manifest_file, plan,
    plan_json, py_to_json, refresh, tree, write, write_json, CRATE, RUST_UNIT,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

fn campaign_error(py: Python<'_>, err: PyErr, message: &str) {
    assert_error(
        py,
        err,
        &module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap(),
        message,
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn string_set(value: &Bound<'_, PyAny>) -> std::collections::BTreeSet<String> {
    value
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().extract::<String>().unwrap())
        .collect()
}

#[test]
fn test_file_recognition_covers_supported_layouts_only() {
    let _case = Case::new();
    Python::attach(|py| {
        let f = generator(py).getattr("_is_test").unwrap();
        for (relative, name) in [
            ("conductor/conftest.py", "conftest.py"),
            ("conductor/test_campaign.py", "test_campaign.py"),
            ("research/tests/check_campaign.py", "check_campaign.py"),
        ] {
            assert!(f.call1((relative, name)).unwrap().is_truthy().unwrap());
        }
        assert!(!f
            .call1(("research/tools/check_campaign.py", "check_campaign.py"))
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}

#[test]
fn rust_planning_intersects_scope_instead_of_expanding_crate() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("crate/Cargo.toml", "[package]\nname = \"crate\"\n"),
            ("crate/src/lib.rs", "#[cfg(test)]\nmod unit {}\n"),
            ("crate/src/sibling.rs", "pub fn sibling() {}\n"),
        ],
    );
    Python::attach(|py| {
        let result = plan_json(
            py,
            "rust",
            case.root(),
            &json!({"day":"20260910","only_sources":["crate/src/lib.rs"]}),
        );
        assert_eq!(result["language"], "rust");
        assert_eq!(result["unpaired"], json!([]));
        assert_eq!(result["unpaired_lines"], 0);
        assert_eq!(
            result["manifests"][0]["generator"]["source"],
            json!(["src/lib.rs"])
        );
    });
}

#[test]
fn crate_with_package_but_no_sources_is_refused() {
    let case = Case::new();
    tree(
        case.root(),
        &[("w/Cargo.toml", "[package]\nname = \"w\"\n")],
    );
    Python::attach(|py| {
        campaign_error(
            py,
            plan(py, "rust", case.root(), &json!({"day":"20260910"})).unwrap_err(),
            "no src",
        )
    });
}

#[test]
fn unmirrored_basename_collision_refuses_both_candidates() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("pkg/subject.py", "x = 1\n"),
            ("elsewhere/test_subject.py", "def test_x(): pass\n"),
            ("further/test_subject.py", "def test_y(): pass\n"),
        ],
    );
    Python::attach(|py| {
        let err = plan(py, "python", case.root(), &json!({"day":"20260910"})).unwrap_err();
        assert!(err.to_string().contains("elsewhere/test_subject.py"));
        assert!(err.to_string().contains("further/test_subject.py"));
    });
}

#[test]
fn tests_mirror_beats_same_basename_stranger() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("pkg/deep/subject.py", "x = 1\n"),
            ("pkg/tests/deep/test_subject.py", "def test_x(): pass\n"),
            ("elsewhere/test_subject.py", "def test_y(): pass\n"),
        ],
    );
    Python::attach(|py| {
        let result = plan_json(py, "python", case.root(), &json!({"day":"20260910"}));
        assert_eq!(result["unpaired"], json!([]));
        assert_eq!(
            &result["manifests"][0]["test_argv"].as_array().unwrap()[5..],
            &json!(["pkg/tests/deep/test_subject.py"])
                .as_array()
                .unwrap()[..]
        );
    });
}

#[test]
fn committed_campaign_subject_is_skipped() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub fn f() -> i32 { 1 }\n#[cfg(test)]\nmod t {}\n",
            ),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            (
                "b/src/lib.rs",
                "pub fn g() -> i32 { 2 }\n#[cfg(test)]\nmod t {}\n",
            ),
        ],
    );
    write_json(
        &case.root().join("conductor/mutation_campaigns/old.json"),
        &json!({
            "mutation_engine":"cargo-mutants","generator":{"source":["src/**/*.rs"],"options":{"package":"a"}}
        }),
    );
    Python::attach(|py| {
        let result = plan_json(py, "rust", case.root(), &json!({"day":"20260907"}));
        assert_eq!(result["already_covered"], json!(["a"]));
        let packages: Vec<_> = result["manifests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["generator"]["options"]["package"].clone())
            .collect();
        assert_eq!(json!(packages), json!(["b"]));
    });
}

#[test]
fn write_refuses_to_replace_recorded_baseline() {
    let case = Case::new();
    fs::create_dir_all(case.root().join("conductor/mutation_campaigns")).unwrap();
    Python::attach(|py| {
        let item = json!({"campaign_id":"x","survivor_baseline":[]});
        let written = write(py, case.root(), std::slice::from_ref(&item), false).unwrap();
        assert_eq!(written, ["conductor/mutation_campaigns/x.json"]);
        campaign_error(
            py,
            write(py, case.root(), std::slice::from_ref(&item), false).unwrap_err(),
            "refusing to replace",
        );
        assert!(!write(py, case.root(), &[item], true).unwrap().is_empty());
        assert!(
            fs::read_to_string(case.root().join("conductor/mutation_campaigns/x.json"))
                .unwrap()
                .ends_with('\n')
        );
    });
}

#[test]
fn generated_rust_manifest_loads_as_generated_campaign() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("crate/Cargo.toml", "[package]\nname = \"crate\"\n"),
            ("crate/src/lib.rs", RUST_UNIT),
        ],
    );
    Python::attach(|py| {
        let item = first(&plan_json(
            py,
            "rust",
            case.root(),
            &json!({"day":"20260907"}),
        ));
        let file = case.root().join("_generated_probe.json");
        write_json(&file, &item);
        let loaded = module(py, "conductor.mutation_engine_generated")
            .getattr("load_generated_campaign")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap();
        assert_eq!(
            loaded
                .getattr("mutation_engine")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "cargo-mutants"
        );
        assert_eq!(
            loaded
                .getattr("language")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "rust"
        );
        assert!(loaded
            .getattr("source_sha256")
            .unwrap()
            .is_truthy()
            .unwrap());
        assert!(!loaded
            .getattr("environment")
            .unwrap()
            .contains("CARGO_TARGET_DIR")
            .unwrap());
    });
}

#[test]
fn generated_python_manifest_runs_tests_naming_its_subject() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("conductor/gate.py", "x = 1\n"),
            ("conductor/test_gate.py", "def test_x(): pass\n"),
            ("research/tools/report.py", "x = 1\n"),
            ("research/tests/test_report.py", "def test_x(): pass\n"),
        ],
    );
    Python::attach(|py| {
        let local = plan_json(py, "python", case.root(), &json!({"day":"20260907"}));
        let sources: Vec<_> = local["manifests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["generator"]["source"][0].clone())
            .collect();
        assert_eq!(
            json!(sources),
            json!(["conductor/gate.py", "research/tools/report.py"])
        );
        for manifest in local["manifests"].as_array().unwrap() {
            assert_names_own_tests(manifest);
        }
        let host = plan_json(py, "python", &repo_root(), &json!({"day":"20260907"}));
        for manifest in host["manifests"].as_array().unwrap() {
            assert_names_own_tests(manifest);
        }
    });
}

fn branch_scope_is_limited(py: Python<'_>, root: &Path) {
    git_repo(
        root,
        &[
            ("pkg/committed.py", "x = 1\n"),
            ("pkg/dirty.py", "x = 1\n"),
            ("pkg/untouched.py", "x = 1\n"),
        ],
    );
    git(root, &["branch", "-q", "base"]);
    fs::write(root.join("pkg/committed.py"), "x = 2\n").unwrap();
    git(root, &["commit", "-qam", "change one file"]);
    fs::write(root.join("pkg/dirty.py"), "x = 3\n").unwrap();
    fs::write(root.join("pkg/brand_new.py"), "x = 4\n").unwrap();
    claim(
        py,
        root,
        "main",
        &["pkg/committed.py", "pkg/dirty.py", "pkg/brand_new.py"],
    );
    let f = generator(py).getattr("changed_sources").unwrap();
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root)).unwrap();
    kw.set_item("owner", "main").unwrap();
    let actual = string_set(&f.call(("base",), Some(&kw)).unwrap());
    assert_eq!(
        actual,
        ["pkg/committed.py", "pkg/dirty.py", "pkg/brand_new.py"]
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    );
    fs::remove_file(root.join("pkg/committed.py")).unwrap();
    let actual = string_set(&f.call(("base",), Some(&kw)).unwrap());
    assert_eq!(
        actual,
        ["pkg/dirty.py", "pkg/brand_new.py"]
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    );
    scoped_test_and_crate_plans(py, root);
}

fn scoped_test_and_crate_plans(py: Python<'_>, root: &Path) {
    tree(
        root,
        &[
            ("conductor/subject.py", "x = 1\n"),
            ("conductor/test_subject.py", "def test_x(): pass\n"),
            ("conductor/bystander.py", "x = 1\n"),
            ("conductor/test_bystander.py", "def test_y(): pass\n"),
        ],
    );
    let python = plan_json(
        py,
        "python",
        root,
        &json!({"day":"20260909","only_sources":["conductor/test_subject.py"]}),
    );
    assert_eq!(
        python["manifests"][0]["generator"]["source"],
        json!(["conductor/subject.py"])
    );
    tree(
        root,
        &[
            ("crates/widget/Cargo.toml", CRATE),
            ("crates/widget/src/lib.rs", RUST_UNIT),
            (
                "crates/other/Cargo.toml",
                &CRATE.replace("widget-core", "other-core"),
            ),
            ("crates/other/src/lib.rs", RUST_UNIT),
        ],
    );
    let rust = plan_json(
        py,
        "rust",
        root,
        &json!({"day":"20260909","only_sources":["crates/widget/src/lib.rs"]}),
    );
    assert_eq!(
        rust["manifests"][0]["generator"]["options"]["package"],
        "widget-core"
    );
    assert_eq!(
        rust["manifests"][0]["generator"]["source"],
        json!(["src/lib.rs"])
    );
    assert_eq!(
        rust["manifests"][0]["source_sha256"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["crates/widget/src/lib.rs"]
    );
    assert!(rust["manifests"][0]["source_sha256"]
        .get("crates/other/src/lib.rs")
        .is_none());
    assert_eq!(
        plan_json(py, "rust", root, &json!({"day":"20260909"}))["manifests"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn shared_dirty_scope_excludes_other_lanes_files() {
    let case = Case::new();
    Python::attach(|py| {
        let root = case.root();
        git_repo(
            root,
            &[("pkg/mine.py", "x = 1\n"), ("pkg/theirs.py", "x = 1\n")],
        );
        fs::write(root.join("pkg/mine.py"), "x = 2\n").unwrap();
        fs::write(root.join("pkg/theirs.py"), "x = 2\n").unwrap();
        claim(py, root, "mine", &["pkg/mine.py"]);
        claim(py, root, "theirs", &["pkg/theirs.py"]);
        let f = generator(py).getattr("changed_sources").unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, root)).unwrap();
        kw.set_item("owner", "mine").unwrap();
        let got = string_set(&f.call(("HEAD",), Some(&kw)).unwrap());
        assert_eq!(
            got,
            ["pkg/mine.py".to_owned()]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        kw.set_item("owner", "missing").unwrap();
        campaign_error(
            py,
            f.call(("HEAD",), Some(&kw)).unwrap_err(),
            "no active ownership claim",
        );
        branch_scope_is_limited(py, &root.join("branch-scope"));
    });
}

#[test]
fn explicit_scope_is_exact_and_ignores_shared_dirty_files() {
    let case = Case::new();
    tree(case.root(), &[("pkg/subject.py", "x = 1\n")]);
    Python::attach(|py| {
        let f = generator(py).getattr("_explicit_scope").unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        let call = |paths: &[&str]| f.call((paths,), Some(&kw));
        assert_eq!(
            call(&["pkg/subject.py", "pkg/subject.py"])
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["pkg/subject.py"]
        );
        assert_eq!(
            call(&["pkg\\subject.py"])
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["pkg/subject.py"]
        );
        for invalid in ["./pkg/subject.py", "../pkg/subject.py", "/pkg/subject.py"] {
            campaign_error(py, call(&[invalid]).unwrap_err(), "repository-relative");
        }
        campaign_error(py, call(&["pkg/absent.py"]).unwrap_err(), "does not exist");
    });
}

#[test]
fn default_campaign_day_is_utc_and_explicit_day_wins() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("pkg/subject.py", "x = 1\n"),
            ("pkg/test_subject.py", "def test_x(): pass\n"),
        ],
    );
    Python::attach(|py| {
        let now = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            let py = args.py();
            assert!(!args.get_item(0)?.is_none());
            let datetime = module(py, "datetime");
            let kw = PyDict::new(py);
            kw.set_item("tzinfo", datetime.getattr("UTC")?)?;
            Ok(datetime
                .getattr("datetime")?
                .call((2026, 1, 2), Some(&kw))?
                .unbind())
        })
        .unwrap();
        let kw = PyDict::new(py);
        kw.set_item("now", now).unwrap();
        let fake = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&kw))
            .unwrap();
        let _patch = AttrPatch::replace(&generator(py), "datetime", &fake);
        let default = first(&plan_json(py, "python", case.root(), &json!({})));
        assert!(default["campaign_id"]
            .as_str()
            .unwrap()
            .ends_with("_20260102"));
        let explicit = first(&plan_json(
            py,
            "python",
            case.root(),
            &json!({"day":"20261231"}),
        ));
        assert!(explicit["campaign_id"]
            .as_str()
            .unwrap()
            .ends_with("_20261231"));
    });
}

fn failed_git(py: Python<'_>, stderr: &str) -> AttrPatch {
    let message = stderr.to_owned();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
            let kw = PyDict::new(args.py());
            kw.set_item("returncode", 1)?;
            kw.set_item("stderr", &message)?;
            kw.set_item("stdout", "")?;
            Ok(module(args.py(), "types")
                .getattr("SimpleNamespace")?
                .call((), Some(&kw))?
                .unbind())
        })
        .unwrap();
    let process = generator(py).getattr("subprocess").unwrap();
    AttrPatch::replace(&process, "run", callback.as_any())
}

#[test]
fn git_scope_failure_without_output_is_actionable() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = failed_git(py, "");
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        campaign_error(
            py,
            generator(py)
                .getattr("changed_sources")
                .unwrap()
                .call(("HEAD",), Some(&kw))
                .unwrap_err(),
            "no output",
        );
    });
}

#[test]
fn git_scope_failure_prefers_stderr_over_empty_stdout() {
    let case = Case::new();
    git_repo(
        &case.root().join("unresolvable-base"),
        &[("pkg/a.py", "x = 1\n")],
    );
    Python::attach(|py| {
        let f = generator(py).getattr("changed_sources").unwrap();
        let kw = PyDict::new(py);
        kw.set_item(
            "repo_root",
            path(py, &case.root().join("unresolvable-base")),
        )
        .unwrap();
        campaign_error(
            py,
            f.call(("no-such-ref",), Some(&kw)).unwrap_err(),
            "cannot determine mutation scope",
        );
        let _patch = failed_git(py, "unknown base");
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        campaign_error(
            py,
            f.call(("HEAD",), Some(&kw)).unwrap_err(),
            "unknown base",
        );
    });
}

#[test]
fn empty_scope_refuses_whole_tree_sweep() {
    let case = Case::new();
    git_repo(case.root(), &[("pkg/a.py", "x = 1\n")]);
    Python::attach(|py| {
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        campaign_error(
            py,
            generator(py)
                .getattr("_branch_scope")
                .unwrap()
                .call(("HEAD",), Some(&kw))
                .unwrap_err(),
            "--all-files",
        );
    });
}

fn rust_manifest(py: Python<'_>, root: &Path, day: &str) -> (Value, PathBuf) {
    tree(
        root,
        &[
            ("crates/widget/Cargo.toml", CRATE),
            ("crates/widget/src/lib.rs", RUST_UNIT),
            ("crates/widget/src/extra.rs", RUST_UNIT),
        ],
    );
    let item = first(&plan_json(py, "rust", root, &json!({"day":day})));
    let written = write(py, root, std::slice::from_ref(&item), false).unwrap();
    (item, manifest_file(root, &written))
}

fn python_manifest(py: Python<'_>, root: &Path) -> (Value, PathBuf) {
    tree(
        root,
        &[
            ("conductor/subject.py", "x = 1\n"),
            (
                "conductor/test_subject.py",
                "def test_subject(): assert True\n",
            ),
        ],
    );
    let item = first(&plan_json(py, "python", root, &json!({"day":"20260910"})));
    let written = write(py, root, std::slice::from_ref(&item), false).unwrap();
    (item, manifest_file(root, &written))
}

#[test]
fn refresh_rebinds_cargo_campaign_to_requested_sources() {
    let case = Case::new();
    Python::attach(|py| {
        let (item, file) = rust_manifest(py, case.root(), "20260909");
        let mut payload = load_json(&file);
        payload["survivor_baseline"] = json!(["known"]);
        payload["survivor_baseline_recorded"] = json!(true);
        payload["generator"]["jobs"] = json!(2);
        payload["generator"]["run_timeout_seconds"] = json!(91);
        write_json(&file, &payload);
        fs::write(
            case.root().join("crates/widget/src/lib.rs"),
            "#[test] fn t() { assert!(true); }\n",
        )
        .unwrap();
        let id = payload["campaign_id"].as_str().unwrap();
        assert_eq!(
            refresh(
                py,
                "refresh_rust_campaign",
                id,
                case.root(),
                Some(&["crates/widget/src/lib.rs"])
            )
            .unwrap(),
            file.strip_prefix(case.root()).unwrap().to_str().unwrap()
        );
        let refreshed = load_json(&file);
        assert_eq!(refreshed["generator"]["source"], json!(["src/lib.rs"]));
        assert_eq!(
            refreshed["source_sha256"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["crates/widget/src/lib.rs"]
        );
        assert_ne!(refreshed["source_sha256"], payload["source_sha256"]);
        assert_eq!(refreshed["test_sha256"], refreshed["source_sha256"]);
        assert_eq!(refreshed["survivor_baseline"], json!(["known"]));
        assert_eq!(refreshed["generator"]["jobs"], 2);
        assert_eq!(refreshed["generator"]["run_timeout_seconds"], 91);
        assert_eq!(
            refreshed["survivor_baseline_note"],
            item["survivor_baseline_note"]
        );
        assert!(fs::read_to_string(&file).unwrap().ends_with('\n'));
        tree(case.root(), &[("elsewhere/lib.rs", RUST_UNIT)]);
        campaign_error(
            py,
            refresh(
                py,
                "refresh_rust_campaign",
                id,
                case.root(),
                Some(&["elsewhere/lib.rs"]),
            )
            .unwrap_err(),
            "elsewhere/lib.rs",
        );
    });
}

fn malformed_implicit_scope(py: Python<'_>, root: &Path) {
    tree(
        root,
        &[
            ("crates/widget/Cargo.toml", CRATE),
            ("crates/widget/src/lib.rs", RUST_UNIT),
        ],
    );
    let mut item = first(&plan_json(py, "rust", root, &json!({"day":"20260909"})));
    item["generator"]["source"] = json!(["../escape.rs"]);
    let written = write(py, root, &[item.clone()], false).unwrap();
    let file = manifest_file(root, &written);
    campaign_error(
        py,
        refresh(
            py,
            "refresh_rust_campaign",
            item["campaign_id"].as_str().unwrap(),
            root,
            None,
        )
        .unwrap_err(),
        "malformed generated Rust source scope",
    );
    assert_eq!(
        load_json(&file)["generator"]["source"],
        json!(["../escape.rs"])
    );
}

fn implicit_scope_case(declared: Value, message: &str) {
    let case = Case::new();
    Python::attach(|py| {
        tree(
            case.root(),
            &[
                ("crates/widget/Cargo.toml", CRATE),
                ("crates/widget/src/lib.rs", RUST_UNIT),
            ],
        );
        let mut item = first(&plan_json(
            py,
            "rust",
            case.root(),
            &json!({"day":"20260909"}),
        ));
        item["generator"]["source"] = declared;
        write(py, case.root(), &[item.clone()], false).unwrap();
        campaign_error(
            py,
            refresh(
                py,
                "refresh_rust_campaign",
                item["campaign_id"].as_str().unwrap(),
                case.root(),
                None,
            )
            .unwrap_err(),
            message,
        );
        malformed_implicit_scope(py, &case.root().join("malformed"));
    });
}

#[test]
fn rust_refresh_rejects_empty_implicit_scope() {
    implicit_scope_case(json!([]), "no non-empty");
}
#[test]
fn rust_refresh_rejects_unbound_implicit_scope() {
    implicit_scope_case(json!(["src/not-generated.rs"]), "outside its current crate");
}

#[test]
fn rust_plan_reports_untested_scoped_crate_with_identity() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            (
                "crates/untested/Cargo.toml",
                &CRATE.replace("widget-core", "untested-core"),
            ),
            ("crates/untested/src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    Python::attach(|py| {
        let result = plan_json(
            py,
            "rust",
            case.root(),
            &json!({"day":"20260910","only_sources":["crates/untested/src/lib.rs"]}),
        );
        assert_eq!(result["manifests"], json!([]));
        assert_eq!(
            result["untested"],
            json!([{"package":"untested-core","root":"crates/untested","lines":1,"reason":"crate untested-core has no tests: no tests/*.rs and no #[cfg(test)] module. A mutation campaign over untested code would report every mutant as survived and prove nothing that reading the crate does not already say."}])
        );
    });
}

#[test]
fn refresh_rebinds_fest_campaign_without_erasing_baseline() {
    let case = Case::new();
    Python::attach(|py| {
        let (item, file) = python_manifest(py, case.root());
        let mut payload = load_json(&file);
        payload["survivor_baseline"] = json!(["engine-recorded"]);
        payload["survivor_baseline_recorded"] = json!(true);
        write_json(&file, &payload);
        tree(
            case.root(),
            &[
                ("conductor/subject.py", "x = 2\n"),
                (
                    "conductor/test_subject.py",
                    "def test_subject(): assert 2 == 2\n",
                ),
            ],
        );
        let id = item["campaign_id"].as_str().unwrap();
        assert_eq!(
            refresh(py, "refresh_python_campaign", id, case.root(), None).unwrap(),
            file.strip_prefix(case.root()).unwrap().to_str().unwrap()
        );
        let mut refreshed = load_json(&file);
        assert!(fs::read_to_string(&file).unwrap().ends_with('\n'));
        assert_eq!(refreshed["survivor_baseline"], json!(["engine-recorded"]));
        assert_eq!(refreshed["survivor_baseline_recorded"], true);
        assert_ne!(refreshed["source_sha256"], payload["source_sha256"]);
        assert_ne!(refreshed["test_sha256"], payload["test_sha256"]);
        refreshed["generator"]["source"] = json!([]);
        write_json(&file, &refreshed);
        campaign_error(
            py,
            refresh(py, "refresh_python_campaign", id, case.root(), None).unwrap_err(),
            "exactly one Python source",
        );
        refreshed["generator"]["source"] = json!(["conductor/absent.py"]);
        write_json(&file, &refreshed);
        campaign_error(
            py,
            refresh(py, "refresh_python_campaign", id, case.root(), None).unwrap_err(),
            "no longer exists in the tree",
        );
    });
}

#[test]
fn refresh_carries_extra_test_campaign_without_erasing_baseline() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("conductor/_bash_quiet.py", "LIMIT = 8000\n"),
            (
                "conductor/test_bash_quiet.py",
                "def test_limit(): assert True\n",
            ),
        ],
    );
    Python::attach(|py| {
        let item = first(&plan_json(
            py,
            "python",
            case.root(),
            &json!({"day":"20260910","extra_tests":{"conductor/_bash_quiet.py":["conductor/test_bash_quiet.py"]}}),
        ));
        let file = manifest_file(
            case.root(),
            &write(py, case.root(), std::slice::from_ref(&item), false).unwrap(),
        );
        let mut payload = load_json(&file);
        payload["survivor_baseline"] = json!(["engine-recorded-survivor"]);
        payload["survivor_baseline_recorded"] = json!(true);
        write_json(&file, &payload);
        fs::write(
            case.root().join("conductor/_bash_quiet.py"),
            "LIMIT = 4000\n",
        )
        .unwrap();
        let id = item["campaign_id"].as_str().unwrap();
        assert_eq!(
            refresh(py, "refresh_python_campaign", id, case.root(), None).unwrap(),
            file.strip_prefix(case.root()).unwrap().to_str().unwrap()
        );
        let refreshed = load_json(&file);
        assert_eq!(
            refreshed["survivor_baseline"],
            json!(["engine-recorded-survivor"])
        );
        assert_eq!(refreshed["survivor_baseline_recorded"], true);
        assert_eq!(
            refreshed["test_argv"],
            json!([
                "python",
                "-m",
                "pytest",
                "-q",
                "--rootdir=.",
                "conductor/test_bash_quiet.py"
            ])
        );
        assert_eq!(
            refreshed["test_sha256"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["conductor/test_bash_quiet.py"]
        );
        assert_ne!(refreshed["source_sha256"], payload["source_sha256"]);
        payload["test_sha256"] = json!({});
        write_json(&file, &payload);
        campaign_error(
            py,
            refresh(py, "refresh_python_campaign", id, case.root(), None).unwrap_err(),
            "no recorded test list",
        );
    });
}

#[test]
fn refresh_refuses_non_generated_cargo_campaign() {
    let case = Case::new();
    let root = case.root();
    write_json(
        &root.join("conductor/mutation_campaigns/hand.json"),
        &json!({"mutation_engine":"reviewed_unified_diff"}),
    );
    Python::attach(|py| {
        campaign_error(
            py,
            refresh(py, "refresh_rust_campaign", "hand", root, None).unwrap_err(),
            "not a cargo-mutants generated campaign",
        );
        campaign_error(
            py,
            refresh(py, "refresh_rust_campaign", "absent", root, None).unwrap_err(),
            "cannot load generated campaign",
        );
        write_json(
            &root.join("conductor/mutation_campaigns/bad-cargo.json"),
            &json!({"mutation_engine":"cargo-mutants","generator":{"options":{"package":"x"}}}),
        );
        campaign_error(
            py,
            refresh(py, "refresh_rust_campaign", "bad-cargo", root, None).unwrap_err(),
            "no generated cargo package",
        );
    });
}

fn preserved_rust_metadata(py: Python<'_>, root: &Path) {
    let (mut item, file) = rust_manifest(py, root, "20260909");
    item["survivor_baseline"] = json!(["known"]);
    item["survivor_baseline_recorded"] = json!(true);
    item["survivor_baseline_note"] = json!("engine note");
    item["survivor_baseline_recorded_at"] = json!("2026-09-09T00:00:00Z");
    write_json(&file, &item);
    let scope = item["generator"]["source"].clone();
    refresh(
        py,
        "refresh_rust_campaign",
        item["campaign_id"].as_str().unwrap(),
        root,
        None,
    )
    .unwrap();
    let refreshed = load_json(&file);
    assert_eq!(refreshed["generator"]["source"], scope);
    for field in [
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ] {
        assert_eq!(refreshed[field], item[field]);
    }
}

#[test]
fn rust_refresh_repairs_legacy_manifest_path_and_retains_note() {
    let case = Case::new();
    Python::attach(|py| {
        let (mut item, file) = rust_manifest(py, case.root(), "20260910");
        item["generator"]["options"]["manifest_path"] = json!("gone/Cargo.toml");
        item["survivor_baseline_note"] = json!("engine-recorded note");
        write_json(&file, &item);
        assert_eq!(
            refresh(
                py,
                "refresh_rust_campaign",
                item["campaign_id"].as_str().unwrap(),
                case.root(),
                None
            )
            .unwrap(),
            file.strip_prefix(case.root()).unwrap().to_str().unwrap()
        );
        let refreshed = load_json(&file);
        assert_eq!(
            refreshed["generator"]["options"]["manifest_path"],
            "crates/widget/Cargo.toml"
        );
        assert_eq!(refreshed["survivor_baseline_note"], "engine-recorded note");
        preserved_rust_metadata(py, &case.root().join("declared-scope"));
    });
}

#[test]
fn rust_refresh_refuses_ambiguous_legacy_package_path() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("a/Cargo.toml", CRATE),
            ("a/src/lib.rs", RUST_UNIT),
            ("b/Cargo.toml", CRATE),
            ("b/src/lib.rs", RUST_UNIT),
        ],
    );
    Python::attach(|py| {
        let mut item = first(&plan_json(
            py,
            "rust",
            case.root(),
            &json!({"day":"20260910"}),
        ));
        item["generator"]["options"]["manifest_path"] = json!("gone/Cargo.toml");
        write(py, case.root(), &[item.clone()], false).unwrap();
        campaign_error(
            py,
            refresh(
                py,
                "refresh_rust_campaign",
                item["campaign_id"].as_str().unwrap(),
                case.root(),
                None,
            )
            .unwrap_err(),
            "no longer exists",
        );
    });
}

#[test]
fn narrow_second_campaign_can_cover_existing_source() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("conductor/subject.py", "x = 1\n"),
            ("conductor/test_subject.py", "def test_x(): pass\n"),
        ],
    );
    write_json(
        &case.root().join("conductor/mutation_campaigns/wide.json"),
        &json!({"campaign_id":"wide","mutation_engine":"fest","generator":{"source":["conductor/subject.py"]}}),
    );
    write_json(
        &case
            .root()
            .join("conductor/mutation_campaigns/registry.json"),
        &json!({"campaigns":[{"manifest":"conductor/mutation_campaigns/wide.json"}]}),
    );
    Python::attach(|py| {
        let scope = json!(["conductor/subject.py"]);
        let skipped = plan_json(
            py,
            "python",
            case.root(),
            &json!({"day":"20260909","only_sources":scope}),
        );
        assert_eq!(skipped["manifests"], json!([]));
        assert_eq!(skipped["already_covered"], scope);
        let narrow = plan_json(
            py,
            "python",
            case.root(),
            &json!({"day":"20260909","only_sources":scope,"include_covered":true}),
        );
        assert_eq!(narrow["manifests"].as_array().unwrap().len(), 1);
        assert_eq!(narrow["manifests"][0]["generator"]["source"], scope);
        assert_eq!(narrow["already_covered"], json!([]));
    });
}

#[test]
fn extra_test_pairs_subject_without_matching_test_name() {
    let case = Case::new();
    tree(
        case.root(),
        &[
            ("research/tools/overrides.py", "x = 1\n"),
            ("research/tools/report.py", "x = 1\n"),
            ("research/tests/test_report.py", "def test_x(): pass\n"),
        ],
    );
    Python::attach(|py| {
        let extra = json!({"research/tools/overrides.py":["research/tests/test_report.py"]});
        let without = plan_json(py, "python", case.root(), &json!({"day":"20260912"}));
        let unpaired: Vec<_> = without["unpaired"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["source"].clone())
            .collect();
        assert_eq!(json!(unpaired), json!(["research/tools/overrides.py"]));
        let result = plan_json(
            py,
            "python",
            case.root(),
            &json!({"day":"20260912","extra_tests":extra}),
        );
        assert_eq!(result["unpaired"], json!([]));
        assert_eq!(result["manifests"].as_array().unwrap().len(), 2);
        let by_source = |name: &str| {
            result["manifests"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["generator"]["source"][0] == name)
                .unwrap()
        };
        assert_eq!(
            &by_source("research/tools/overrides.py")["test_argv"]
                .as_array()
                .unwrap()[5..],
            &json!(["research/tests/test_report.py"]).as_array().unwrap()[..]
        );
        assert_eq!(
            by_source("research/tools/overrides.py")["test_sha256"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["research/tests/test_report.py"]
        );
        assert_eq!(
            &by_source("research/tools/report.py")["test_argv"]
                .as_array()
                .unwrap()[5..],
            &json!(["research/tests/test_report.py"]).as_array().unwrap()[..]
        );
        campaign_error(
            py,
            plan(
                py,
                "python",
                case.root(),
                &json!({"extra_tests":{"research/tools/gone.py":[]}}),
            )
            .unwrap_err(),
            "names no python subject",
        );
        campaign_error(
            py,
            plan(py, "rust", case.root(), &json!({"extra_tests":extra})).unwrap_err(),
            "python subjects only",
        );
        verify_extra_test_cli(py, case.root(), &extra);
    });
}

fn verify_extra_test_cli(py: Python<'_>, root: &Path, extra: &Value) {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, root)).unwrap();
    let parse = generator(py).getattr("_extra_tests").unwrap();
    let got = py_to_json(
        &parse
            .call(
                (vec![
                    "research/tools/overrides.py=research/tests/test_report.py",
                ],),
                Some(&kw),
            )
            .unwrap(),
    );
    assert_eq!(&got, extra);
    campaign_error(
        py,
        parse
            .call(
                (vec!["research/tools/overrides.py=research/tools/report.py"],),
                Some(&kw),
            )
            .unwrap_err(),
        "not a test file",
    );
    campaign_error(
        py,
        parse
            .call((vec!["research/tools/overrides.py"],), Some(&kw))
            .unwrap_err(),
        "SOURCE=TEST",
    );
}
