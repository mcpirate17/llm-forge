use super::*;
use crate::mutation_run_test_support::{change, git as run_git, repository, write};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn host(files: &[&str]) -> tempfile::TempDir {
    let dir = repository(files);
    change(dir.path(), files);
    dir
}

fn output(root: &Path, args: &[&str]) -> String {
    let raw = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(raw.status.success(), "{args:?}: {raw:?}");
    String::from_utf8_lossy(&raw.stdout).trim().to_string()
}

fn object_files(repo: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo.join(".git/objects"), &mut found);
    found.sort();
    found
}

fn read(root: &Path, relative: &str) -> String {
    fs::read_to_string(root.join(relative)).unwrap()
}

#[test]
fn a_snapshot_reproduces_the_dirty_tree_on_top_of_head() {
    let dir = host(&["src/a.py", "src/gone.py"]);
    let repo = dir.path();
    write(repo, "src/a.py", "x = 3\n");
    fs::remove_file(repo.join("src/gone.py")).unwrap();
    write(repo, "src/new.py", "y = 1\n");
    let snapshot = Snapshot::create(repo).unwrap();
    assert_eq!(read(&snapshot.worktree, "src/a.py"), "x = 3\n");
    assert_eq!(read(&snapshot.worktree, "src/new.py"), "y = 1\n");
    assert!(!snapshot.worktree.join("src/gone.py").exists());
    assert_eq!(snapshot.included_untracked, ["src/new.py"]);
    assert_eq!(
        output(&snapshot.worktree, &["rev-parse", "HEAD^"]),
        output(repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        output(&snapshot.worktree, &["rev-parse", "HEAD"]),
        snapshot.commit
    );
    assert_eq!(output(&snapshot.worktree, &["status", "--porcelain"]), "");
}

#[test]
fn building_a_snapshot_changes_nothing_in_the_host_repository() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    write(repo, "src/a.py", "x = 3\n");
    write(repo, "src/new.py", "y = 1\n");
    let objects = object_files(repo);
    let status = output(repo, &["status", "--porcelain"]);
    let head = output(repo, &["rev-parse", "HEAD"]);
    let snapshot = Snapshot::create(repo).unwrap();
    assert_eq!(object_files(repo), objects);
    assert_eq!(output(repo, &["status", "--porcelain"]), status);
    assert_eq!(output(repo, &["rev-parse", "HEAD"]), head);
    snapshot.remove().unwrap();
}

#[test]
fn a_snapshot_of_a_linked_worktree_leaves_the_shared_objects_alone() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    let linked_parent = tempfile::tempdir().unwrap();
    let linked = linked_parent.path().join("linked");
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    write(&linked, "src/a.py", "x = 9\n");
    let objects = object_files(repo);
    let snapshot = Snapshot::create(&linked).unwrap();
    assert_eq!(read(&snapshot.worktree, "src/a.py"), "x = 9\n");
    assert_eq!(object_files(repo), objects);
}

#[test]
fn generated_files_stay_out_and_test_material_rides_along_whatever_its_suffix() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    write(repo, ".gitignore", "ignored.py\n");
    write(repo, "ignored.py", "");
    write(repo, "build.log", "");
    write(repo, "src/model.bin", "");
    write(repo, "tests/golden.dat", "");
    write(repo, "src/fixtures/input.bin", "");
    write(repo, "src/test_data/case.bin", "");
    assert_eq!(
        snapshot_untracked_paths(repo).unwrap(),
        [
            "src/fixtures/input.bin",
            "src/test_data/case.bin",
            "tests/golden.dat"
        ]
    );
}

#[test]
fn receipts_reports_and_task_notes_never_ride_along() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    write(
        repo,
        "pyproject.toml",
        "[tool.conductor]\nmutation_registry = \"lanes/registry.json\"\n",
    );
    run_git(repo, &["add", "pyproject.toml"]);
    run_git(repo, &["commit", "-q", "-m", "config"]);
    write(repo, "lanes/receipts/run.json", "{}");
    write(repo, "research/reports/out.md", "");
    write(repo, "tasks/note.md", "");
    write(repo, "lanes/kept.json", "{}");
    assert_eq!(snapshot_untracked_paths(repo).unwrap(), ["lanes/kept.json"]);
}

#[test]
fn extra_suffixes_are_configurable_and_normalized() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    write(
        repo,
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_extra_suffixes = [\"CSV\", \".proto\"]\n",
    );
    run_git(repo, &["add", "pyproject.toml"]);
    run_git(repo, &["commit", "-q", "-m", "config"]);
    write(repo, "data/a.csv", "");
    write(repo, "data/b.proto", "");
    write(repo, "data/c.bin", "");
    assert_eq!(
        snapshot_untracked_paths(repo).unwrap(),
        ["data/a.csv", "data/b.proto"]
    );
}

#[test]
fn malformed_extra_suffixes_are_refused() {
    let dir = host(&["src/a.py"]);
    let repo = dir.path();
    for (body, expected) in [
        ("\"csv\"", "must be a list of file suffixes"),
        ("[1]", "must be a list of file suffixes"),
        ("[\" \"]", "must not contain an empty suffix"),
    ] {
        write(
            repo,
            "pyproject.toml",
            &format!("[tool.conductor]\nsnapshot_extra_suffixes = {body}\n"),
        );
        let error = snapshot_untracked_paths(repo).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn a_failed_snapshot_removes_its_own_directory() {
    let dir = tempfile::tempdir().unwrap();
    let before = snapshot_roots();
    let error = Snapshot::create(dir.path()).unwrap_err();
    assert!(error.starts_with("snapshot preparation"), "{error}");
    assert!(snapshot_roots().is_subset(&before));
}

fn snapshot_roots() -> BTreeSet<PathBuf> {
    fs::read_dir(env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("llm-orchestrator-snapshot-")
        })
        .map(|entry| entry.path())
        .collect()
}

#[test]
fn remove_deletes_the_snapshot_and_dropping_one_is_the_backstop() {
    let dir = host(&["src/a.py"]);
    let removed = Snapshot::create(dir.path()).unwrap();
    let root = removed.root.clone();
    assert!(root.join("worktree/src/a.py").is_file());
    removed.remove().unwrap();
    assert!(!root.exists());
    let dropped = Snapshot::create(dir.path()).unwrap();
    let root = dropped.root.clone();
    drop(dropped);
    assert!(!root.exists());
}

#[test]
fn a_snapshot_that_survives_removal_says_so() {
    let dir = host(&["src/a.py"]);
    let snapshot = Snapshot::create(dir.path()).unwrap();
    let root = snapshot.root.clone();
    let held = root.join("worktree/src");
    fs::set_permissions(&held, fs::Permissions::from_mode(0o500)).unwrap();
    let error = snapshot.remove().unwrap_err();
    fs::set_permissions(&held, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert!(error.contains("survived removal"), "{error}");
}

#[test]
fn the_exported_interpreter_is_the_hosts_choice_then_its_venv() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    write(repo, ".venv/bin/python", "");
    assert_eq!(
        snapshot_python(repo).unwrap(),
        repo.join(".venv/bin/python").to_string_lossy()
    );
    write(
        repo,
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_python = \"/opt/python\"\n",
    );
    assert_eq!(snapshot_python(repo).unwrap(), "/opt/python");
}

#[test]
fn a_blank_or_non_text_interpreter_override_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for body in ["\" \"", "3"] {
        write(
            dir.path(),
            "pyproject.toml",
            &format!("[tool.conductor]\nsnapshot_python = {body}\n"),
        );
        let error = snapshot_python(dir.path()).unwrap_err();
        assert!(
            error.contains("must be a path to a Python interpreter"),
            "{error}"
        );
    }
}

#[test]
fn only_a_regular_executable_file_counts() {
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("tool");
    fs::write(&tool, "").unwrap();
    assert!(!is_executable(&tool));
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(is_executable(&tool));
    assert!(!is_executable(dir.path()));
    assert!(!is_executable(&dir.path().join("absent")));
}

#[test]
fn optional_guest_never_discovers_a_host_venv_and_validates_explicit_configuration() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(configured_snapshot_python(dir.path()).unwrap(), None);
    write(dir.path(), ".venv/bin/python", "");
    assert_eq!(configured_snapshot_python(dir.path()).unwrap(), None);
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.conductor]\nsnapshot_python='/explicit/guest'\n",
    );
    assert_eq!(
        configured_snapshot_python(dir.path()).unwrap(),
        Some("/explicit/guest".into())
    );
    assert_eq!(snapshot_python(dir.path()).unwrap(), "/explicit/guest");
    for value in ["3", "''", "' '"] {
        write(
            dir.path(),
            "pyproject.toml",
            &format!("[tool.conductor]\nsnapshot_python={value}\n"),
        );
        assert!(configured_snapshot_python(dir.path()).is_err());
        assert!(snapshot_python(dir.path()).is_err());
    }
}

#[test]
fn configured_filter_is_never_executed_and_conversion_is_refused() {
    let dir = host(&["src/a.py"]);
    let root = dir.path();
    let marker = root.join("filter-marker");
    let repository = git2::Repository::open(root).unwrap();
    let mut config = repository.config().unwrap();
    let command = format!("touch {}", marker.display());
    for key in [
        "filter.probe.clean",
        "filter.probe.smudge",
        "filter.probe.process",
    ] {
        config.set_str(key, &command).unwrap();
    }
    config.set_bool("filter.probe.required", true).unwrap();
    write(root, ".gitattributes", "*.py filter=probe\n");
    let before_index = fs::read(root.join(".git/index")).unwrap();
    let before_objects = object_files(root);
    let error = Snapshot::create(root).unwrap_err();
    assert!(
        error.contains("Git attributes input is unsupported"),
        "{error}"
    );
    assert!(!marker.exists());
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), before_index);
    assert_eq!(object_files(root), before_objects);
}

#[test]
fn configured_hooks_templates_and_fsmonitor_do_not_run() {
    let dir = host(&["src/a.py"]);
    let root = dir.path();
    let marker = root.join("hook-marker");
    let hooks = root.join("host-hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let repository = git2::Repository::open(root).unwrap();
    let mut config = repository.config().unwrap();
    config
        .set_str("core.hooksPath", hooks.to_str().unwrap())
        .unwrap();
    config
        .set_str("core.fsmonitor", hook.to_str().unwrap())
        .unwrap();
    config
        .set_str("init.templateDir", hooks.to_str().unwrap())
        .unwrap();
    let snapshot = Snapshot::create(root).unwrap();
    assert!(!marker.exists());
    snapshot.remove().unwrap();
}

#[test]
fn staged_addition_is_not_an_untracked_overlay_and_head_overlay_is_live() {
    let dir = host(&["src/a.py"]);
    let root = dir.path();
    write(root, "src/staged.py", "staged\n");
    run_git(root, &["add", "src/staged.py"]);
    write(root, "src/a.py", "staged version\n");
    run_git(root, &["add", "src/a.py"]);
    write(root, "src/a.py", "working version\n");
    let before_index = fs::read(root.join(".git/index")).unwrap();
    let snapshot = Snapshot::create(root).unwrap();
    assert_eq!(read(&snapshot.worktree, "src/a.py"), "working version\n");
    assert!(!snapshot.worktree.join("src/staged.py").exists());
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), before_index);
}

#[test]
fn raw_snapshot_preserves_executable_mode_and_relative_symlink() {
    use std::os::unix::fs::symlink;
    let dir = host(&["src/a.py"]);
    let root = dir.path();
    fs::set_permissions(root.join("src/a.py"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("a.py", root.join("src/alias.py")).unwrap();
    let snapshot = Snapshot::create(root).unwrap();
    let mode = fs::metadata(snapshot.worktree.join("src/a.py"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
    assert_eq!(
        fs::read_link(snapshot.worktree.join("src/alias.py")).unwrap(),
        Path::new("a.py")
    );
}

#[test]
fn sparse_index_and_symlinked_ancestors_refuse_without_following() {
    use std::os::unix::fs::symlink;
    let dir = host(&["src/a.py"]);
    let repo = git2::Repository::open(dir.path()).unwrap();
    let mut config = repo.config().unwrap();
    config.set_bool("core.sparsecheckout", true).unwrap();
    assert!(Snapshot::create(dir.path())
        .unwrap_err()
        .contains("core.sparsecheckout"));
    config.set_bool("core.sparsecheckout", false).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("a.py"), "outside").unwrap();
    fs::rename(dir.path().join("src"), dir.path().join("old-src")).unwrap();
    symlink(outside.path(), dir.path().join("src")).unwrap();
    assert!(Snapshot::create(dir.path()).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("a.py")).unwrap(),
        "outside"
    );
}

#[test]
fn held_ignore_rules_preserve_negation_and_parent_pruning() {
    let dir = host(&["src/a.py"]);
    let root = dir.path();
    write(root, ".gitignore", "ignored/\n*.tmp\n!keep.tmp\n");
    write(root, ".git/info/exclude", "info-only.py\n");
    write(root, "ignored/hidden.py", "secret\n");
    write(root, "tests/discard.tmp", "ignored\n");
    write(root, "tests/keep.tmp", "keep\n");
    write(root, "info-only.py", "ignored\n");
    let paths = snapshot_untracked_paths(root).unwrap();
    assert_eq!(paths, ["tests/keep.tmp"]);
}

#[test]
fn held_ignore_file_and_absence_drift_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".gitignore");
    fs::write(&path, "*.tmp\n").unwrap();
    let mut inputs = ignore_rules::Inputs::new();
    inputs.read(&path).unwrap().unwrap();
    let absent = dir.path().join(".gitattributes");
    inputs.require_absent(&absent).unwrap();
    inputs.verify().unwrap();
    fs::write(&path, "*.rs\n").unwrap();
    assert!(inputs.verify().is_err());
    let mut inputs = ignore_rules::Inputs::new();
    inputs.require_absent(&absent).unwrap();
    fs::write(&absent, "*.rs filter=x\n").unwrap();
    assert!(inputs.verify().is_err());
}

#[test]
fn config_include_header_refusal_ignores_innocent_words() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config");
    fs::write(
        &file,
        "# include is only a comment\n[user]\nname = include maintainer\n",
    )
    .unwrap();
    let mut inputs = ignore_rules::Inputs::new();
    native::bounded_config(&file, &mut inputs).unwrap();
    for header in [
        "[include]",
        "\u{feff}[include]",
        "[ INCLUDE ]",
        "[includeIf \"gitdir:somewhere\"]",
        "[ Include.path ]",
    ] {
        fs::write(&file, format!("{header}\npath = somewhere\n")).unwrap();
        let mut inputs = ignore_rules::Inputs::new();
        assert!(
            native::bounded_config(&file, &mut inputs)
                .unwrap_err()
                .to_string()
                .contains("includes"),
            "{header}"
        );
    }
}
