#![cfg(feature = "python-compat-tests")]
//! Changed-campaign guard: native assertions at the public Python and CLI seams.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::{module, Case};

const ROOT: &str = "conductor/mutation_campaigns/";

fn campaign(engine: Option<&str>) -> String {
    match engine {
        Some(engine) => {
            format!(r#"{{"campaign_id":"x","title":"t","mutation_engine":"{engine}"}}"#)
        }
        None => r#"{"campaign_id":"x","title":"t"}"#.into(),
    }
}

fn call_git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn repo(case: &Case) -> (PathBuf, String) {
    let root = case.mkdir("repo/conductor/mutation_campaigns");
    let repo = root.parent().unwrap().parent().unwrap().to_owned();
    fs::write(
        root.join("old.json"),
        campaign(Some("reviewed_unified_diff")),
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
        vec!["add", "--", "conductor/mutation_campaigns/old.json"],
        vec!["commit", "-qm", "base"],
    ] {
        call_git(&repo, &args);
    }
    let base = String::from_utf8(call_git(&repo, &["rev-parse", "HEAD"]).stdout).unwrap();
    fs::write(
        root.join("new.json"),
        campaign(Some("reviewed_unified_diff")),
    )
    .unwrap();
    let generated = Python::attach(|py| {
        let engines = module(py, "conductor.check_generated_mutants")
            .getattr("generated_engines")
            .unwrap()
            .call0()
            .unwrap();
        engines
            .extract::<std::collections::HashSet<String>>()
            .unwrap()
            .into_iter()
            .min()
            .expect("runner must declare a generated engine")
    });
    fs::write(root.join("gen.json"), campaign(Some(&generated))).unwrap();
    (repo, base.trim().to_owned())
}

fn guard(repo: &Path, base: &str, files: &[&str]) -> Output {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let python = std::env::var_os("PYO3_PYTHON").unwrap_or_else(|| "python3".into());
    let mut import_paths = vec![source];
    if let Some(paths) = std::env::var_os("PYTHONPATH") {
        import_paths.extend(std::env::split_paths(&paths));
    }
    Command::new(python)
        .arg("-m")
        .arg("conductor.check_generated_mutants")
        .arg("--repo")
        .arg(repo)
        .arg("--base")
        .arg(base)
        .args(files)
        .env("PYTHONPATH", std::env::join_paths(import_paths).unwrap())
        .current_dir(repo)
        .output()
        .unwrap()
}

#[test]
fn engine_classifier_admits_only_generated_direct_campaign_manifests() {
    let _case = Case::new();
    Python::attach(|py| {
        let check = module(py, "conductor.check_generated_mutants");
        let classify = check.getattr("declared_engine").unwrap();
        let engines = check.getattr("generated_engines").unwrap().call0().unwrap();
        let runner = module(py, "conductor.mutation_engine_generated");
        assert!(engines
            .eq(runner.getattr("GENERATED_ENGINES").unwrap())
            .unwrap());
        assert!(engines.len().unwrap() > 0);
        for engine in ["reviewed_unified_diff", "reviewed_patch", "diff", "patch"] {
            let got: String = classify
                .call1((format!("{ROOT}x.json"), campaign(Some(engine))))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(got, engine);
        }
        for engine in engines.try_iter().unwrap() {
            let engine: String = engine.unwrap().extract().unwrap();
            assert!(classify
                .call1((format!("{ROOT}x.json"), campaign(Some(&engine))))
                .unwrap()
                .is_none());
        }
        assert_eq!(
            classify
                .call1((format!("{ROOT}x.json"), campaign(None)))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "<none declared>"
        );
        for path in [
            "conductor/mutation_campaigns/registry.json",
            "conductor/mutation_campaigns/reproducibility_baseline.json",
            "conductor/mutation_campaigns/receipts/x.json",
            "conductor/mutation_campaigns/registry.d/x.json",
            "conductor/mutation_campaigns/patches/x/1.patch.json",
            "research/reports/x.json",
            "conductor/mutation_campaigns/x.md",
        ] {
            let payload = if path.ends_with("registry.json") || path.ends_with("baseline.json") {
                "{\"schema_version\":\"v1\"}".to_owned()
            } else {
                campaign(Some("diff"))
            };
            assert!(classify.call1((path, payload)).unwrap().is_none(), "{path}");
        }
        assert!(classify
            .call1((format!("{ROOT}x.json"), "not json at all"))
            .unwrap()
            .is_none());
    });
}

#[test]
fn argument_and_base_lookup_fail_closed() {
    let case = Case::new();
    let (repo, base) = repo(&case);
    Python::attach(|py| {
        let check = module(py, "conductor.check_generated_mutants");
        let parse = check.getattr("parse_argv").unwrap();
        let parsed = parse
            .call1((vec!["--repo", "/r", "--base", "abc", "a.json", "b.json"],))
            .unwrap();
        assert_eq!(
            parsed.get_item(0).unwrap().extract::<String>().unwrap(),
            "/r"
        );
        assert_eq!(
            parsed.get_item(1).unwrap().extract::<String>().unwrap(),
            "abc"
        );
        assert_eq!(
            parsed
                .get_item(2)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["a.json", "b.json"]
        );
        let parsed = parse
            .call1((vec!["--base", "abc", "--repo", "/r"],))
            .unwrap();
        assert_eq!(parsed.get_item(2).unwrap().len().unwrap(), 0);
        for args in [vec!["a.json"], vec!["--repo", "/r", "a.json"]] {
            let err = parse.call1((args,)).unwrap_err();
            assert!(err
                .matches(py, py.get_type::<pyo3::exceptions::PySystemExit>())
                .unwrap());
        }
        let at_base = check.getattr("paths_at_base").unwrap();
        let old = format!("{ROOT}old.json");
        let new = format!("{ROOT}new.json");
        let found: Vec<String> = at_base
            .call1((
                repo.to_str().unwrap(),
                base.as_str(),
                vec![old.as_str(), new.as_str()],
            ))
            .unwrap()
            .extract::<std::collections::HashSet<String>>()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(found, vec![old.clone()]);
        assert_eq!(
            at_base
                .call1((repo.to_str().unwrap(), base.as_str(), Vec::<String>::new()))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        let err = at_base
            .call1((repo.to_str().unwrap(), "not-a-ref", vec![old]))
            .unwrap_err();
        assert!(err.to_string().contains("ls-tree"));
    });
}

#[test]
fn changed_campaign_cli_distinguishes_existing_generated_and_new_handwritten() {
    let case = Case::new();
    let (repo, base) = repo(&case);
    let old = "conductor/mutation_campaigns/old.json";
    let new = "conductor/mutation_campaigns/new.json";
    let generated = "conductor/mutation_campaigns/gen.json";
    fs::write(repo.join(old), r#"{"campaign_id":"x","mutation_engine":"reviewed_unified_diff","source_sha256":{"a.py":"beef"}}"#).unwrap();
    for paths in [vec![old], vec![generated], vec![]] {
        let output = guard(&repo, &base, &paths);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{paths:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = guard(&repo, &base, &[new]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("new.json"));
    let output = guard(&repo, &base, &[generated, old, new]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("new.json"));
    assert!(
        !err.contains("gen.json") && !err.contains("old.json"),
        "{err}"
    );
    let output = guard(
        &repo,
        &base,
        &["conductor/mutation_campaigns/gone.json", new],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("new.json"));
}
