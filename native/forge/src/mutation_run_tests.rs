use super::*;
use crate::mutation_run_test_support::{campaign, manifest, write};

const STAMP: BuildStamp = BuildStamp {
    forge_version: "0.0.0",
    source_sha256: "source",
    git_commit: "commit",
    source_matches_git: true,
};

fn result(stdout: &str, stderr: &str) -> CommandResult {
    CommandResult {
        returncode: Some(1),
        timed_out: false,
        duration_seconds: 0.0,
        stdout_tail: stdout.to_string(),
        stderr_tail: stderr.to_string(),
    }
}

/// A repository whose declared interpreter sits beside a `fest`.
fn host(dir: &Path) -> PathBuf {
    let root = absolute_resolved(dir).unwrap();
    let bin = root.join("bin");
    write(&bin, "fest", "");
    fs::set_permissions(
        bin.join("fest"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    write(
        &root,
        "pyproject.toml",
        &format!(
            "[tool.conductor]\nsnapshot_python = \"{}/python\"\n",
            bin.display()
        ),
    );
    root
}

fn pinned_host(dir: &Path) -> (PathBuf, Campaign) {
    let root = host(dir);
    write(&root, "src/a.py", "x = 1\n");
    let pin = sha256_path(&root.join("src/a.py")).unwrap();
    let subject = campaign(&root, json!({"source_sha256": {"src/a.py": pin}}));
    (root, subject)
}

#[test]
fn a_receipt_names_the_compiled_runner_and_no_python_file() {
    let dir = tempfile::tempdir().unwrap();
    let (root, subject) = pinned_host(dir.path());
    let receipt = open_receipt(
        &subject,
        &root,
        Some("/venv/bin/python"),
        &STAMP,
        1_700_000_000.0,
    )
    .unwrap();
    assert_eq!(receipt["schema_version"], RECEIPT_SCHEMA);
    assert_eq!(receipt["status"], RUNNING);
    assert_eq!(receipt["manifest"], "campaign.json");
    assert_eq!(receipt["manifest_sha256"], subject.manifest_sha256.as_str());
    assert_eq!(receipt["mutants_are_generated"], true);
    assert_eq!(receipt["runner_kind"], RUNNER_KIND);
    assert_eq!(receipt["interpreter"], "/venv/bin/python");
    let runner = &receipt["native_runner"];
    assert_eq!(runner["schema_version"], RUNNER_SCHEMA);
    assert_eq!(runner["compiled_source_sha256"], "source");
    assert_eq!(runner["compiled_git_commit"], "commit");
    assert_eq!(runner["source_matches_git"], true);
    assert_eq!(
        runner["executable_sha256"],
        sha256_path(Path::new("/proc/self/exe")).unwrap()
    );
    assert!(runner.get("components_sha256").is_none());
    for absent in [
        "runner_sha256",
        "runner_components_sha256",
        "adapter_sha256",
        "core_sha256",
    ] {
        assert!(!receipt.contains_key(absent), "{absent}");
    }
}

#[test]
fn a_manifest_outside_the_repository_is_refused() {
    let repository = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let subject = campaign(elsewhere.path(), json!({}));
    let root = absolute_resolved(repository.path()).unwrap();
    assert_eq!(
        open_receipt(&subject, &root, Some("/python"), &STAMP, 0.0).unwrap_err(),
        "campaign manifest must be inside the repository"
    );
}

#[test]
fn a_receipt_path_resolves_inside_the_repository_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let root = absolute_resolved(dir.path()).unwrap();
    let subject = campaign(&root, json!({}));
    let (path, relative) =
        resolve_receipt_path(&subject, Some(Path::new("out/r.json")), &root, 0.0).unwrap();
    assert_eq!(path, root.join("out/r.json"));
    assert_eq!(relative, "out/r.json");
    let absolute = root.join("r.json");
    let (_, relative) = resolve_receipt_path(&subject, Some(&absolute), &root, 0.0).unwrap();
    assert_eq!(relative, "r.json");
    for outside in [Path::new("/elsewhere/r.json"), Path::new("../r.json")] {
        assert_eq!(
            resolve_receipt_path(&subject, Some(outside), &root, 0.0).unwrap_err(),
            "receipt path must be inside the repository"
        );
    }
}

#[test]
fn a_default_receipt_lands_in_the_receipt_root_under_a_stamped_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = absolute_resolved(dir.path()).unwrap();
    let subject = campaign(&root, json!({}));
    let (path, relative) = resolve_receipt_path(&subject, None, &root, 1_700_000_000.0).unwrap();
    assert!(path.parent().unwrap().is_dir(), "the directory is created");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("fixture_") && name.ends_with("Z.json"),
        "{name}"
    );
    assert!(relative.ends_with(&name));
}

#[test]
fn default_receipt_refuses_traversal_and_symlinks_before_creating_directories() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut subject = campaign(root, json!({}));
    subject.campaign_id = "../../../escaped".into();
    assert!(resolve_receipt_path(&subject, None, root, 0.0)
        .unwrap_err()
        .contains("campaign_id"));
    assert!(!root.join("research").exists());

    subject.campaign_id = "fixture".into();
    write(
        root,
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root='link/nested'\n",
    );
    std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
    let error = resolve_receipt_path(&subject, None, root, 0.0).unwrap_err();
    assert!(error.contains("inside the repository"), "{error}");
    let explicit = Path::new("missing/../link/nested/receipt.json");
    let error = resolve_receipt_path(&subject, Some(explicit), root, 0.0).unwrap_err();
    assert!(error.contains("inside the repository"), "{error}");
    assert!(!root.join("missing").exists());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn a_baseline_that_could_not_start_python_says_how_to_fix_it() {
    for marker in INTERPRETER_FAILURES {
        assert!(interpreter_hint(&result(marker, "")).is_some(), "{marker}");
        assert!(interpreter_hint(&result("", marker)).is_some(), "{marker}");
    }
    assert_eq!(interpreter_hint(&result("1 failed", "")), None);
}

#[test]
fn last_chars_counts_characters_not_bytes() {
    assert_eq!(last_chars("héllo", 4), "éllo");
    assert_eq!(last_chars("ab", 9), "ab");
}

#[test]
fn a_cancelled_run_is_closed_as_cancelled_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("receipt.json");
    let mut receipt = crate::mutation_run_test_support::receipt(&[("a", KILLED)]);
    close_stopped(&mut receipt, &output, Some(3), Stop::Cancelled(15)).unwrap();
    assert_eq!(receipt["status"], CANCELLED);
    assert_eq!(receipt["cancelled_by_signal"], 15);
    assert_eq!(receipt["accounting"]["planned"], 3);
    assert_eq!(receipt["accounting"]["unstarted"], 2);
    let written: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(written["status"], CANCELLED);
}

#[test]
fn a_refused_run_is_closed_as_an_error_and_stays_refused() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("receipt.json");
    let mut receipt = crate::mutation_run_test_support::receipt(&[]);
    let closed = close_stopped(&mut receipt, &output, None, Stop::Refused("why".into()));
    assert_eq!(closed, Err(Stop::Refused("why".into())));
    assert_eq!(receipt["status"], ERROR);
    assert_eq!(receipt["error"], "why");
    let written: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(written["status"], ERROR);
}

#[test]
fn a_status_already_decided_survives_the_close() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("receipt.json");
    let mut receipt = crate::mutation_run_test_support::receipt(&[]);
    receipt.insert("status".into(), json!(BASELINE_FAILED));
    let closed = close_stopped(&mut receipt, &output, None, Stop::Refused("red".into()));
    assert!(closed.is_err());
    assert_eq!(receipt["status"], BASELINE_FAILED);
}

#[test]
fn a_run_is_refused_before_anything_loads_without_the_opt_in() {
    let request = RunRequest {
        manifest: PathBuf::from("/nonexistent/manifest.json"),
        repo_root: PathBuf::from("/nonexistent"),
        owner: "lane".into(),
        base: "main".into(),
        only: Vec::new(),
        allow_mutations: false,
        receipt: None,
        stamp: STAMP,
    };
    assert_eq!(
        run_campaign(&request).unwrap_err(),
        Stop::Refused("refusing mutation run without --allow-mutations".into())
    );
}

#[test]
fn a_campaign_with_its_engine_and_pinned_bytes_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let (root, subject) = pinned_host(dir.path());
    let report = inspect(&subject, &root).unwrap();
    assert_eq!(report["status"], "READY");
    assert_eq!(report["readiness_reasons"], json!([]));
    assert_eq!(
        report["engine_binary"],
        root.join("bin/fest").to_string_lossy().as_ref()
    );
}

#[test]
fn inspection_names_every_reason_a_campaign_could_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let (root, subject) = pinned_host(dir.path());
    write(&root, "src/a.py", "x = 2\n");
    assert_eq!(
        inspect(&subject, &root).unwrap()["readiness_reasons"],
        json!(["pinned source drifted: src/a.py"])
    );
    fs::remove_file(root.join("src/a.py")).unwrap();
    let report = inspect(&subject, &root).unwrap();
    assert_eq!(report["status"], "NOT_READY");
    assert_eq!(
        report["readiness_reasons"],
        json!(["pinned source is absent: src/a.py"])
    );
}

#[test]
fn inspection_names_a_missing_engine() {
    let dir = tempfile::tempdir().unwrap();
    let root = absolute_resolved(dir.path()).unwrap();
    write(&root, "src/a.py", "");
    let pin = sha256_path(&root.join("src/a.py")).unwrap();
    write(
        &root,
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_python = \"/nowhere/python\"\n",
    );
    let path = manifest(&root, json!({"source_sha256": {"src/a.py": pin}}));
    let subject = Campaign::load(&path).unwrap();
    let report = inspect(&subject, &root).unwrap();
    if crate::mutation_snapshot::on_path("fest").is_none() {
        assert_eq!(report["status"], "NOT_READY");
        assert!(report["engine_binary"].is_null());
        assert!(report["readiness_reasons"][0]
            .as_str()
            .unwrap()
            .contains("fest is not installed"));
    }
}

#[test]
fn only_fest_discovers_python_and_native_receipts_record_no_guest() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), ".venv/bin/python", "");
    for engine in ["cargo-mutants", "mull"] {
        let subject = campaign(dir.path(), json!({"mutation_engine": engine}));
        assert_eq!(selected_interpreter(&subject, dir.path()).unwrap(), None);
        let receipt = open_receipt(&subject, dir.path(), None, &STAMP, 0.0).unwrap();
        assert!(receipt["interpreter"].is_null());
    }
    let subject = campaign(dir.path(), json!({}));
    assert_eq!(
        selected_interpreter(&subject, dir.path()).unwrap(),
        Some(
            dir.path()
                .join(".venv/bin/python")
                .to_string_lossy()
                .into_owned()
        )
    );
    assert!(engine_binary(&subject, None)
        .unwrap_err()
        .contains("requires a Python guest"));
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_python='/configured/python'\n",
    );
    for engine in ["fest", "cargo-mutants", "mull"] {
        let subject = campaign(dir.path(), json!({"mutation_engine": engine}));
        assert_eq!(
            selected_interpreter(&subject, dir.path()).unwrap(),
            Some("/configured/python".into())
        );
    }
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_python=3\n",
    );
    for engine in ["fest", "cargo-mutants", "mull"] {
        let subject = campaign(dir.path(), json!({"mutation_engine": engine}));
        assert!(selected_interpreter(&subject, dir.path()).is_err());
    }
}

#[test]
fn guest_selection_clears_inherited_and_campaign_environment_without_losing_cache_isolation() {
    const CHILD: &str = "FORGE_OPTIONAL_PYTHON_GUEST_CHILD";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mutation_run::tests::guest_selection_clears_inherited_and_campaign_environment_without_losing_cache_isolation", "--nocapture"])
            .env(CHILD, "1").env(SNAPSHOT_PYTHON_ENV, "/stale/inherited")
            .output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (interpreter, expected) in [
        (None, "absent"),
        (Some("/selected/python"), "/selected/python"),
    ] {
        crate::mutation_run_test_support::with_run(dir.path(), interpreter, json!({}), |run| {
            let environment =
                BTreeMap::from([(SNAPSHOT_PYTHON_ENV.into(), "/stale/campaign".into())]);
            let argv = ["/bin/sh", "-c", "test -d \"$PYTHONPYCACHEPREFIX\" && printf '%s' \"${CONDUCTOR_SNAPSHOT_PYTHON-absent}\""]
                .map(str::to_string);
            let (result, stdout) = run.run(&argv, dir.path(), 5, &environment, &[]).unwrap();
            assert!(result.succeeded());
            assert_eq!(stdout, expected);
        });
    }
}
