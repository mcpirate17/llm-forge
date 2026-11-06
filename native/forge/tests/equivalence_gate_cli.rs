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
    fn command(&self, module: &str) -> Command {
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
            .args(["--module", module, "--grants"])
            .arg(&self.grants)
            .arg("--guest-path")
            .arg(&self.guest)
            .args(["--guest-sha256", &"0".repeat(64), "--json"]);
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
