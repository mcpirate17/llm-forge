use super::*;
use git2::{Repository, Signature, Time};
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    process::Command,
    time::Duration,
};

fn isolated(case: &str) {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "test_report_worker::tests::isolated_child",
        "--test-threads=1",
        "--nocapture",
    ]);
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("FORGE_TEST_REPORT_WORKER_CASE", case)
        .env("FORGE_TEST_REPORT_WORKER_ROOT", root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path());
    let output =
        crate::process_probe::run(&mut command, Duration::from_secs(15), 128 * 1024).unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

fn fixture(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let repo = Repository::init(&root).unwrap();
    fs::write(root.join("subject.py"), "VALUE=1\n").unwrap();
    fs::write(root.join("test_subject.py"), "import missing_local\n").unwrap();
    let mut index = repo.index().unwrap();
    for name in ["subject.py", "test_subject.py"] {
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::new("fixture", "fixture@example.invalid", &Time::new(0, 0)).unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "authored fixture",
        &tree,
        &[],
    )
    .unwrap();
    root
}

fn args(root: &Path, config: &Path, max_bytes: usize) -> WorkerArgs {
    WorkerArgs {
        root: root.to_owned(),
        config_root: config.to_owned(),
        max_bytes,
    }
}

#[test]
fn worker_reports_shared_classification_without_artifacts() {
    isolated("report");
}
#[test]
fn worker_refuses_git_indirection_and_semantic_includes() {
    isolated("controls");
}
#[test]
fn worker_requires_exact_roots_and_empty_private_config() {
    isolated("roots");
}
#[test]
fn worker_enforces_output_budget_and_rejects_git_environment() {
    isolated("budgets");
}

#[test]
fn isolated_child() {
    let Ok(case) = std::env::var("FORGE_TEST_REPORT_WORKER_CASE") else {
        return;
    };
    let parent = PathBuf::from(std::env::var_os("FORGE_TEST_REPORT_WORKER_ROOT").unwrap());
    let config = parent.join("empty-config");
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    // Poisoned synthetic home/XDG configs must never be consulted by the worker.
    fs::write(parent.join(".gitconfig"), "not valid git configuration\n").unwrap();
    let xdg = parent.join("git");
    fs::create_dir(&xdg).unwrap();
    fs::write(xdg.join("config"), "also invalid git configuration\n").unwrap();
    // This single-test child isolates libgit2 before even creating authored Git
    // fixtures. No shared libtest process mutates global Git search options.
    isolate_configuration(&config).unwrap();
    for level in [
        ConfigLevel::System,
        ConfigLevel::Global,
        ConfigLevel::XDG,
        ConfigLevel::ProgramData,
    ] {
        // SAFETY: same isolated single-test child; no concurrent Git accesses.
        let actual = unsafe { git2::opts::get_search_path(level).unwrap() };
        assert_eq!(Path::new(actual.to_str().unwrap()), config);
    }
    match case.as_str() {
        "report" => report(&parent, &config),
        "controls" => controls(&parent, &config),
        "roots" => roots(&parent, &config),
        "budgets" => budgets(&parent, &config),
        _ => panic!("unexpected isolated worker case"),
    }
}

fn report(parent: &Path, config: &Path) {
    let root = fixture(parent, "report");
    let output = execute(args(&root, config, 262144)).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["report"]["tests_scanned"], 1);
    assert_eq!(value["report"]["modules_scanned"], 2);
    assert_eq!(value["root"], root.to_str().unwrap());
    assert_eq!(value["advisory"], true);
    assert_eq!(
        output,
        crate::test_report::readonly_exact(&root, 262144).unwrap()
    );
    assert!(
        matches!(fs::symlink_metadata(root.join("tasks")), Err(error)
        if error.kind() == std::io::ErrorKind::NotFound)
    );
    // The word "include" in an ordinary value/comment is not an include key.
    fs::write(root.join(".git/config"), "[core]\nrepositoryformatversion=0\n# include.path ignored comment\n[custom]\ntext=include.path\n").unwrap();
    execute(args(&root, config, 262144)).unwrap();
}

fn controls(parent: &Path, config: &Path) {
    for (number, control) in [
        "commondir",
        "gitdir",
        "config.worktree",
        "objects/info/alternates",
        "objects/info/http-alternates",
        "info/grafts",
        "refs/replace",
    ]
    .into_iter()
    .enumerate()
    {
        let root = fixture(parent, &format!("control{number}"));
        let path = root.join(".git").join(control);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "outside\n").unwrap();
        assert!(execute(args(&root, config, 262144))
            .unwrap_err()
            .to_string()
            .contains("Git control"));
    }
    for (number, text) in [
        "[include]\npath=missing-config\n",
        "[includeIf \"gitdir:never/\"]\npath=missing-config\n",
        "[core]\nexcludesFile=/outside/ignore\n",
    ]
    .into_iter()
    .enumerate()
    {
        let root = fixture(parent, &format!("include{number}"));
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.join(".git/config"))
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
        assert!(execute(args(&root, config, 262144)).is_err());
    }
}

fn roots(parent: &Path, config: &Path) {
    let root = fixture(parent, "roots");
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    assert!(execute(args(&nested, config, 262144)).is_err());
    let alias = parent.join("alias");
    symlink(&root, &alias).unwrap();
    assert!(execute(args(&alias, config, 262144)).is_err());
    fs::rename(root.join(".git"), parent.join("external-git")).unwrap();
    fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", parent.join("external-git").display()),
    )
    .unwrap();
    assert!(execute(args(&root, config, 262144)).is_err());
    let root = fixture(parent, "config-root");
    fs::set_permissions(config, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(execute(args(&root, config, 262144))
        .unwrap_err()
        .to_string()
        .contains("private"));
    fs::set_permissions(config, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(config.join(".gitconfig"), "[log]\nshowRoot=false\n").unwrap();
    assert!(execute(args(&root, config, 262144))
        .unwrap_err()
        .to_string()
        .contains("empty"));
}

fn budgets(parent: &Path, config: &Path) {
    let root = fixture(parent, "budgets");
    for maximum in [0, 511, 262145] {
        assert!(execute(args(&root, config, maximum)).is_err());
    }
    assert!(execute(args(&root, config, 512))
        .unwrap_err()
        .to_string()
        .contains("output budget"));
    std::env::set_var("GIT_CONFIG_COUNT", "0");
    assert!(execute(args(&root, config, 262144))
        .unwrap_err()
        .to_string()
        .contains("Git environment"));
    std::env::remove_var("GIT_CONFIG_COUNT");
}
