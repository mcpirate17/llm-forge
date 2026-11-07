//! Real CLI admission and committed/live input controls; no guest execution.
#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
use git2::{Repository, Signature};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    root: tempfile::TempDir,
    repository: Repository,
    commit: String,
    guest: PathBuf,
    grants: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repository = Repository::init(root.path().join("repo")).unwrap();
        let repo = repository.workdir().unwrap();
        fs::write(repo.join("probe.py"), "def value(x):\n    return x + 1\n").unwrap();
        fs::write(
            repo.join("pyproject.toml"),
            "[tool.forge.equivalence_probe]\njobs=1\n",
        )
        .unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("probe.py")).unwrap();
        index.add_path(Path::new("pyproject.toml")).unwrap();
        index.write().unwrap();
        let tree = index.write_tree().unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "authored commit",
                &repository.find_tree(tree).unwrap(),
                &[],
            )
            .unwrap()
            .to_string();
        fs::create_dir(root.path().join("scratch")).unwrap();
        fs::create_dir(root.path().join("cgroup")).unwrap();
        let guest = root.path().join("not-an-executable-guest.so");
        fs::write(&guest, "authored data, never loaded").unwrap();
        fs::set_permissions(&guest, fs::Permissions::from_mode(0o600)).unwrap();
        let forge = PathBuf::from(forge_binary::path());
        let python = Path::new("/usr/bin/false").canonicalize().unwrap();
        let grants = root.path().join("grants.json");
        fs::write(&grants, serde_json::to_vec(&json!({
            "version":1,"source_root":repo,"read_only":[repo,root.path(),forge.parent(),python.parent()],
            "programs":{"forge":forge,"python":python},"python":"python","environment":{},
            "scratch_parent":root.path().join("scratch"),"cgroup_parent":root.path().join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":512,"processes":16},
            "max_wall_seconds":60,"max_cpu_seconds":30,"max_address_space_mib":1024
        })).unwrap()).unwrap();
        fs::set_permissions(&grants, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            root,
            repository,
            commit,
            guest,
            grants,
        }
    }
    fn base_command(&self) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .env("PATH", "")
            .env("HOME", self.root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["equivalence", "gate", "--root"])
            .arg(self.repository.workdir().unwrap())
            .arg("--grants")
            .arg(&self.grants)
            .arg("--guest-path")
            .arg(&self.guest)
            .args(["--guest-sha256", &"0".repeat(64), "--json"]);
        command
    }
    fn command(&self, module: &str) -> Command {
        let mut command = self.base_command();
        command.args(["--module", module]);
        command
    }
    fn invoke(&self, commit: &str, module: &str, extra: &[&str]) -> Output {
        self.command(module)
            .args(["--commit", commit])
            .args(extra)
            .output()
            .unwrap()
    }
    fn invoke_live(&self, inputs: &[&str]) -> Output {
        let mut command = self.command("probe.py");
        command.arg("--live");
        for input in inputs {
            command.args(["--input", input]);
        }
        command.output().unwrap()
    }
    fn make_gate(&self, extra: &[&str]) -> Output {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        Command::new("make")
            .current_dir(self.repository.workdir().unwrap())
            .env_remove("MAKEFLAGS")
            .env("HOME", self.root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["--no-print-directory", "-f"])
            .arg(repository.join("conductor.mk"))
            .args([
                "slop-gate",
                "SLOP_BASE=HEAD",
                "PYTHON=/nonexistent/forbidden-python",
            ])
            .arg(format!("FORGE={}", forge_binary::path()))
            .arg(format!("SLOP_GRANTS={}", self.grants.display()))
            .arg(format!("SLOP_GUEST_PATH={}", self.guest.display()))
            .arg(format!("SLOP_GUEST_SHA256={}", "0".repeat(64)))
            .args(extra)
            .output()
            .unwrap()
    }
    fn preserved(&self, head: &[u8], index: &[u8], live_config: &[u8]) {
        assert_eq!(fs::read(self.repository.path().join("HEAD")).unwrap(), head);
        assert_eq!(
            fs::read(self.repository.path().join("index")).unwrap(),
            index
        );
        assert_eq!(
            fs::read(self.repository.workdir().unwrap().join("pyproject.toml")).unwrap(),
            live_config
        );
        assert_eq!(
            fs::read_dir(self.root.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn unchanged_tracked_selection_returns_empty_summary_without_loading_guest() {
    let f = Fixture::new();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let config = fs::read(f.repository.workdir().unwrap().join("pyproject.toml")).unwrap();
    let output = f
        .base_command()
        .args([
            "--live",
            "--tracked-only",
            "--changed-from",
            "HEAD",
            "--enforce",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["modules"], json!([]));
    assert_eq!(report["execution_skipped"], "no_selected_modules");
    assert_eq!(report["execution_verified"], false);
    assert_eq!(
        report["summary"],
        serde_json::to_value(slop_core::gate_types::Summary::default()).unwrap()
    );
    assert_eq!(report["capture_cleanup_complete"], true);
    assert_eq!(report["live_capture_cleanup_complete"], true);
    assert_eq!(report["live_inputs"]["profile"], "tracked-live-inputs-v1");
    assert!(report.get("execution").is_none());
    f.preserved(&head, &index, &config);
}

#[test]
fn empty_selection_still_refuses_invalid_captured_configuration() {
    let f = Fixture::new();
    let config = b"[tool.forge.equivalence_probe]\nper_module_timeout=0\n";
    fs::write(
        f.repository.workdir().unwrap().join("pyproject.toml"),
        config,
    )
    .unwrap();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let output = f
        .base_command()
        .args(["--live", "--tracked-only", "--changed-from", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("module timeout"),
        "{output:?}"
    );
    assert!(output.stdout.is_empty());
    f.preserved(&head, &index, config);
}

#[test]
fn multiple_modules_validate_all_targets_before_guest_admission() {
    let f = Fixture::new();
    let output = f
        .command("probe.py")
        .args(["--commit", &f.commit, "--module", "missing.py"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("absent"),
        "{output:?}"
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read_dir(f.root.path().join("scratch")).unwrap().count(),
        0
    );
}

#[test]
fn all_tracked_and_changed_modules_reach_guest_pin_admission() {
    let f = Fixture::new();
    fs::write(
        f.repository.workdir().unwrap().join("probe.py"),
        "def value(x):\n    return x + 8\n",
    )
    .unwrap();
    for mode in [vec!["--all-tracked"], vec!["--changed-from", "HEAD"]] {
        let output = f
            .base_command()
            .args(["--live", "--tracked-only"])
            .args(mode)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("digest") || error.contains("SHA-256"),
            "{error}"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(
            fs::read_dir(f.root.path().join("scratch")).unwrap().count(),
            0
        );
    }
}

#[test]
fn make_gate_preserves_explicit_output_authority_and_native_empty_result() {
    let f = Fixture::new();
    let artifacts = f.root.path().join("gate findings");
    fs::create_dir(&artifacts).unwrap();
    fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
    let config = format!(
        "[tool.forge.equivalence_probe]\nrun_artifacts='{}'\n",
        artifacts.display()
    );
    fs::write(
        f.repository.workdir().unwrap().join("pyproject.toml"),
        &config,
    )
    .unwrap();
    let absent = f.make_gate(&[]);
    assert!(!absent.status.success(), "{absent:?}");
    assert!(
        String::from_utf8_lossy(&absent.stderr).contains("artifact"),
        "{absent:?}"
    );
    assert_eq!(fs::read_dir(&artifacts).unwrap().count(), 0);
    let accepted = f.make_gate(&[&format!("SLOP_ARTIFACTS={}", artifacts.display())]);
    assert!(accepted.status.success(), "{accepted:?}");
    let files: Vec<_> = fs::read_dir(&artifacts).unwrap().collect();
    assert_eq!(files.len(), 1);
    let summary: serde_json::Value =
        serde_json::from_slice(&fs::read(files[0].as_ref().unwrap().path()).unwrap()).unwrap();
    assert_eq!(
        summary,
        serde_json::to_value(slop_core::gate_types::Summary::default()).unwrap()
    );
    assert_eq!(
        fs::read_dir(f.root.path().join("scratch")).unwrap().count(),
        0
    );
}

#[test]
fn make_gate_requires_authority_and_forwards_selection_without_python() {
    let f = Fixture::new();
    let missing = f.make_gate(&["SLOP_GRANTS="]);
    assert!(!missing.status.success(), "{missing:?}");
    assert!(String::from_utf8_lossy(&missing.stdout).contains("Set SLOP_GRANTS"));
    for flags in [
        vec!["SLOP_ALL_TRACKED=1"],
        vec!["SLOP_MODULES=probe.py", "SLOP_INPUTS=probe.py"],
    ] {
        let output = f.make_gate(&flags);
        assert!(!output.status.success(), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("digest") || error.contains("SHA-256"),
            "{output:?}"
        );
        assert!(!error.contains("forbidden-python"), "{error}");
        assert_eq!(
            fs::read_dir(f.root.path().join("scratch")).unwrap().count(),
            0
        );
    }
}

#[test]
fn explicit_commit_reaches_guest_pin_admission_despite_invalid_live_configuration() {
    let f = Fixture::new();
    let config = b"[invalid and deliberately unparseable live TOML";
    fs::write(
        f.repository.workdir().unwrap().join("pyproject.toml"),
        config,
    )
    .unwrap();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let output = f.invoke(&f.commit, "probe.py", &[]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("digest") || error.contains("SHA-256"),
        "{error}"
    );
    assert!(output.stdout.is_empty());
    f.preserved(&head, &index, config);
}

#[test]
fn invalid_commit_module_limits_and_partial_record_grants_refuse_without_publication() {
    let f = Fixture::new();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let config = fs::read(f.repository.workdir().unwrap().join("pyproject.toml")).unwrap();
    for (commit, module, extra, expected) in [
        ("HEAD", "probe.py", vec![], "full lowercase"),
        (
            f.commit.as_str(),
            "../probe.py",
            vec![],
            "unsafe Git tree path",
        ),
        (f.commit.as_str(), "absent.py", vec![], "absent"),
        (
            f.commit.as_str(),
            "probe.py",
            vec!["--wall-seconds", "61"],
            "wall limit",
        ),
        (f.commit.as_str(), "probe.py", vec!["--jobs", "0"], "jobs"),
    ] {
        let output = f.invoke(commit, module, &extra);
        assert_eq!(output.status.code(), Some(4), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{output:?}"
        );
        assert!(output.stdout.is_empty());
        f.preserved(&head, &index, &config);
    }
    let output = f.invoke(
        &f.commit,
        "probe.py",
        &["--store", "/explicit/record.sqlite"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--store-sha256"));
    f.preserved(&head, &index, &config);
}

#[test]
fn live_selection_uses_dirty_configuration_and_preserves_host_on_refusal() {
    let f = Fixture::new();
    let repo = f.repository.workdir().unwrap();
    let config = b"[invalid and deliberately unparseable live TOML";
    fs::write(repo.join("pyproject.toml"), config).unwrap();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let output = f.invoke_live(&["probe.py"]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("TOML") || error.contains("table header"),
        "{error}"
    );
    assert!(!error.contains("guest SHA-256"), "{error}");
    assert!(output.stdout.is_empty());
    f.preserved(&head, &index, config);
}

#[test]
fn live_ignored_binary_data_reaches_guest_pin_but_empty_inputs_refuse() {
    let f = Fixture::new();
    let repo = f.repository.workdir().unwrap();
    fs::write(repo.join("probe.py"), "def value(x):\n    return x + 71\n").unwrap();
    fs::write(repo.join(".gitignore"), "private-data/\n").unwrap();
    fs::create_dir(repo.join("private-data")).unwrap();
    fs::write(repo.join("private-data/input"), [0, 255, 17, 34]).unwrap();
    let head = fs::read(f.repository.path().join("HEAD")).unwrap();
    let index = fs::read(f.repository.path().join("index")).unwrap();
    let config = fs::read(repo.join("pyproject.toml")).unwrap();
    let output = f.invoke_live(&["probe.py", "private-data"]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("digest") || error.contains("SHA-256"),
        "{error}"
    );
    assert!(output.stdout.is_empty());
    f.preserved(&head, &index, &config);

    fs::create_dir(repo.join("empty-input")).unwrap();
    let refused = f.invoke_live(&["probe.py", "empty-input"]);
    assert_eq!(refused.status.code(), Some(4), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("empty"),
        "{refused:?}"
    );
    assert!(refused.stdout.is_empty());
    f.preserved(&head, &index, &config);
    assert_eq!(
        fs::read(repo.join("private-data/input")).unwrap(),
        [0, 255, 17, 34]
    );
    assert_eq!(
        fs::read_to_string(repo.join("probe.py")).unwrap(),
        "def value(x):\n    return x + 71\n"
    );
}
