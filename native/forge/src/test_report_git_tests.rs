use super::*;
use git2::{RepositoryInitOptions, Signature, Time};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

struct Fixture {
    _root: tempfile::TempDir,
    work: PathBuf,
    git: PathBuf,
}
impl Fixture {
    fn new(external: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();
        let repo = if external {
            let mut options = RepositoryInitOptions::new();
            options.no_dotgit_dir(true).mkpath(true).workdir_path(&work);
            Repository::init_opts(root.path().join("metadata"), &options).unwrap()
        } else {
            Repository::init(&work).unwrap()
        };
        repo.set_head("refs/heads/main").unwrap();
        let git = repo.path().canonicalize().unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = repo.config().unwrap();
        config.set_bool("diff.renames", true).unwrap();
        config.set_i64("diff.renameLimit", 1000).unwrap();
        config.set_bool("log.showRoot", true).unwrap();
        config.set_str("diff.algorithm", "myers").unwrap();
        let excludes = root.path().join("empty-excludes");
        fs::write(&excludes, "").unwrap();
        config
            .set_str("core.excludesFile", excludes.to_str().unwrap())
            .unwrap();
        Self {
            _root: root,
            work,
            git,
        }
    }
    fn repo(&self) -> Repository {
        Repository::open(&self.git).unwrap()
    }
    fn source(&self) -> Source {
        Source::open(&self.work).unwrap()
    }
    fn write(&self, name: &str, bytes: &str) {
        let path = self.work.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn commit(
        &self,
        changes: &[(&str, Option<&str>)],
        parents: &[Oid],
        author: Time,
        seconds: i64,
    ) -> Oid {
        let repo = self.repo();
        let mut index = repo.index().unwrap();
        index.clear().unwrap();
        if let Some(parent) = parents.first() {
            index
                .read_tree(&repo.find_commit(*parent).unwrap().tree().unwrap())
                .unwrap();
        }
        for (name, content) in changes {
            if let Some(content) = content {
                self.write(name, content);
                index.add_path(Path::new(name)).unwrap();
            } else {
                index.remove_path(Path::new(name)).unwrap();
                fs::remove_file(self.work.join(name)).unwrap();
            }
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let commits: Vec<_> = parents
            .iter()
            .map(|id| repo.find_commit(*id).unwrap())
            .collect();
        let references: Vec<_> = commits.iter().collect();
        let author = Signature::new("author", "author@example.invalid", &author).unwrap();
        let committer = Signature::new(
            "committer",
            "committer@example.invalid",
            &Time::new(seconds, 0),
        )
        .unwrap();
        let id = repo
            .commit(
                None,
                &author,
                &committer,
                "authored fixture",
                &tree,
                &references,
            )
            .unwrap();
        repo.reference("refs/heads/main", id, true, "authored fixture")
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        id
    }
}
fn day(month: u32, date: u32) -> i64 {
    crate::civil::days_from_civil(2026, month, date) * 86_400
}

#[test]
fn latest_path_uses_author_timezone_not_greatest_author_date_or_committer_date() {
    let f = Fixture::new(false);
    let root = f.commit(
        &[
            ("keep.py", Some("keep=1\n")),
            ("subject.py", Some("value=1\n")),
        ],
        &[],
        Time::new(day(9, 12) + 1800, -60),
        day(9, 12),
    );
    let next = f.commit(
        &[("subject.py", Some("value=2\n"))],
        &[root],
        Time::new(day(9, 8), 0),
        day(9, 13),
    );
    f.commit(
        &[("subject.py", Some("value=3\n"))],
        &[next],
        Time::new(day(9, 7), 0),
        day(9, 13),
    );
    let inventory = Inventory::capture(&f.source()).unwrap();
    assert_eq!(inventory.last_commit_dates["keep.py"], "2026-09-11");
    assert_eq!(inventory.last_commit_dates["subject.py"], "2026-09-07");
    assert!(!inventory.shallow);
}

#[test]
fn merge_contributions_are_omitted_but_both_parent_histories_are_walked() {
    let f = Fixture::new(false);
    let root = f.commit(
        &[("subject.py", Some("value=0\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    let left = f.commit(
        &[
            ("subject.py", Some("value=1\n")),
            ("left.py", Some("left=1\n")),
        ],
        &[root],
        Time::new(day(1, 5), 0),
        day(1, 20),
    );
    let right = f.commit(
        &[
            ("subject.py", Some("value=2\n")),
            ("right.py", Some("right=1\n")),
        ],
        &[root],
        Time::new(day(1, 4), 0),
        day(1, 21),
    );
    f.commit(
        &[
            ("subject.py", Some("value=3\n")),
            ("right.py", Some("right=1\n")),
        ],
        &[left, right],
        Time::new(day(1, 25), 0),
        day(1, 25),
    );
    let inventory = Inventory::capture(&f.source()).unwrap();
    assert_eq!(inventory.last_commit_dates["subject.py"], "2026-01-04");
    assert_eq!(inventory.last_commit_dates["left.py"], "2026-01-05");
    assert_eq!(inventory.last_commit_dates["right.py"], "2026-01-04");
}

#[test]
fn untracked_python_tests_respect_nested_ignore_and_index_membership() {
    let f = Fixture::new(false);
    f.commit(
        &[
            (".gitignore", Some("ignored/\n")),
            ("test_tracked.py", Some("pass\n")),
        ],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    f.write("tests/test_new.py", "pass\n");
    f.write("nested/case_test.py", "pass\n");
    f.write("ignored/test_skip.py", "pass\n");
    f.write("plain.py", "pass\n");
    fs::write(f.git.join("info/exclude"), "local/\n").unwrap();
    f.write("local/test_skip.py", "pass\n");
    fs::remove_file(f.work.join("test_tracked.py")).unwrap();
    let inventory = Inventory::capture(&f.source()).unwrap();
    assert_eq!(inventory.tracked, [".gitignore", "test_tracked.py"]);
    assert_eq!(
        inventory.untracked_tests,
        ["nested/case_test.py", "tests/test_new.py"]
    );
    f.write("tests/test_added.py", "pass\n");
    assert!(inventory
        .verify(&f.source())
        .unwrap_err()
        .to_string()
        .contains("inventory changed"));
}

#[test]
fn rename_dates_follow_name_only_semantics_and_disabled_detection_is_explicit() {
    let f = Fixture::new(false);
    let root = f.commit(
        &[("old.py", Some("value=42\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    f.commit(
        &[("old.py", None), ("new.py", Some("value=42\n"))],
        &[root],
        Time::new(day(2, 1), 0),
        day(2, 1),
    );
    let source = f.source();
    let inventory = Inventory::capture(&source).unwrap();
    assert_eq!(inventory.last_commit_dates["old.py"], "2026-01-01");
    assert_eq!(inventory.last_commit_dates["new.py"], "2026-02-01");
    f.repo()
        .config()
        .unwrap()
        .set_bool("diff.renames", false)
        .unwrap();
    assert!(inventory.verify(&source).is_err());
    let without = Inventory::capture(&f.source()).unwrap();
    assert_eq!(without.last_commit_dates["old.py"], "2026-02-01");
}

#[test]
fn unicode_history_paths_are_raw_and_root_visibility_is_configured() {
    let f = Fixture::new(false);
    f.commit(
        &[("src/café.py", Some("value=1\n"))],
        &[],
        Time::new(day(3, 1), 0),
        day(3, 1),
    );
    let inventory = Inventory::capture(&f.source()).unwrap();
    assert_eq!(inventory.last_commit_dates["src/café.py"], "2026-03-01");
    assert!(inventory
        .last_commit_dates
        .keys()
        .all(|path| !path.starts_with('"')));
    f.repo()
        .config()
        .unwrap()
        .set_bool("log.showRoot", false)
        .unwrap();
    assert!(inventory.verify(&f.source()).is_err());
    assert!(Inventory::capture(&f.source())
        .unwrap()
        .last_commit_dates
        .is_empty());
}

#[test]
fn shallow_boundary_is_a_root_and_mutating_boundary_metadata_invalidates_capture() {
    let f = Fixture::new(false);
    let root = f.commit(
        &[("keep.py", Some("keep=1\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    let next = f.commit(
        &[("next.py", Some("next=1\n"))],
        &[root],
        Time::new(day(2, 1), 0),
        day(2, 1),
    );
    fs::write(f.git.join("shallow"), format!("{next}\n")).unwrap();
    let source = f.source();
    let inventory = Inventory::capture(&source).unwrap();
    assert!(inventory.shallow);
    assert_eq!(inventory.last_commit_dates["keep.py"], "2026-02-01");
    fs::write(f.git.join("shallow"), "").unwrap();
    assert!(inventory.verify(&source).is_err());
}

#[test]
fn shallow_merge_boundary_contributes_its_tree_despite_raw_parent_count() {
    let f = Fixture::new(false);
    let root = f.commit(
        &[("keep.py", Some("keep=1\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    let left = f.commit(
        &[("left.py", Some("left=1\n"))],
        &[root],
        Time::new(day(2, 1), 0),
        day(2, 1),
    );
    let right = f.commit(
        &[("right.py", Some("right=1\n"))],
        &[root],
        Time::new(day(2, 2), 0),
        day(2, 2),
    );
    let merge = f.commit(
        &[("right.py", Some("right=1\n"))],
        &[left, right],
        Time::new(day(3, 1), 0),
        day(3, 1),
    );
    fs::write(f.git.join("shallow"), format!("{merge}\n")).unwrap();
    let inventory = Inventory::capture(&f.source()).unwrap();
    assert_eq!(inventory.last_commit_dates["keep.py"], "2026-03-01");
    assert_eq!(inventory.last_commit_dates.len(), 3);
}

#[test]
fn held_head_index_and_external_git_directory_are_bound_without_history_replay() {
    let f = Fixture::new(true); // Separate ordinary Git directory, never a worktree or clone.
    let first = f.commit(
        &[("subject.py", Some("value=1\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    let source = f.source();
    let inventory = Inventory::capture(&source).unwrap();
    inventory.verify(&source).unwrap();
    f.write("added.py", "value=2\n");
    let repo = f.repo();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("added.py")).unwrap();
    index.write().unwrap();
    assert!(inventory.verify(&source).is_err());
    let second = f.commit(
        &[("added.py", Some("value=2\n"))],
        &[first],
        Time::new(day(2, 1), 0),
        day(2, 1),
    );
    let moved = Inventory::capture(&f.source()).unwrap();
    assert_eq!(moved.head, second.to_string());
    repo.set_head_detached(first).unwrap();
    assert!(moved.verify(&source).is_err());
}

#[test]
fn unborn_history_and_unsupported_rename_modes_refuse_loudly() {
    let f = Fixture::new(false);
    assert!(Inventory::capture(&f.source())
        .err()
        .unwrap()
        .to_string()
        .contains("HEAD"));
    f.commit(
        &[("subject.py", Some("value=1\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    f.repo()
        .config()
        .unwrap()
        .set_str("diff.renames", "copies")
        .unwrap();
    assert!(Inventory::capture(&f.source())
        .err()
        .unwrap()
        .to_string()
        .contains("diff.renames"));
}

#[test]
fn oversized_rename_work_refuses_instead_of_returning_unmatched_path_dates() {
    let f = Fixture::new(false);
    let old: Vec<_> = (0..257).map(|n| format!("old{n}.py")).collect();
    let create: Vec<_> = old
        .iter()
        .map(|name| (name.as_str(), Some("old=1\n")))
        .collect();
    let first = f.commit(&create, &[], Time::new(day(1, 1), 0), day(1, 1));
    let new: Vec<_> = (0..257).map(|n| format!("new{n}.py")).collect();
    let mut replace: Vec<_> = old.iter().map(|name| (name.as_str(), None)).collect();
    replace.extend(new.iter().map(|name| (name.as_str(), Some("new=2\n"))));
    f.commit(&replace, &[first], Time::new(day(2, 1), 0), day(2, 1));
    assert!(Inventory::capture(&f.source())
        .err()
        .unwrap()
        .to_string()
        .contains("rename candidate work"));
}

// Environment changes live only in a bounded, single-test child process.
fn isolated_environment(case: &str) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let xdg = root.path().join("xdg");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&xdg).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "test_report_git::tests::environment_child",
        "--test-threads=1",
        "--nocapture",
    ]);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("FORGE_TEST_REPORT_GIT_CASE", case)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
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

#[test]
fn unsupported_repository_and_inline_configuration_overrides_refuse_capture_and_verify() {
    isolated_environment("overrides");
}

#[test]
fn explicit_configuration_isolation_and_alternate_index_are_honored() {
    isolated_environment("configuration");
}

#[test]
fn environment_child() {
    let Ok(case) = std::env::var("FORGE_TEST_REPORT_GIT_CASE") else {
        return;
    };
    let f = Fixture::new(false);
    f.commit(
        &[("test_subject.py", Some("value=1\n"))],
        &[],
        Time::new(day(1, 1), 0),
        day(1, 1),
    );
    match case.as_str() {
        "overrides" => override_refusals(&f),
        "configuration" => configuration_isolation(&f),
        _ => panic!("unknown isolated Git case"),
    }
}

fn override_refusals(f: &Fixture) {
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
    ] {
        let source = f.source();
        let held = Inventory::capture(&source).unwrap();
        std::env::set_var(name, "unsupported");
        let error = Inventory::capture(&source).err().unwrap().to_string();
        assert!(error.contains(name), "{error}");
        let error = held.verify(&source).err().unwrap().to_string();
        assert!(error.contains(name), "{error}");
        std::env::remove_var(name);
    }
}

fn configuration_isolation(f: &Fixture) {
    let mut config = f.repo().config().unwrap();
    config.remove("log.showRoot").unwrap();
    let global = f._root.path().join("global-config");
    let system = f._root.path().join("system-config");
    fs::write(&global, "[log]\nshowRoot=false\n").unwrap();
    fs::write(&system, "[log]\nshowRoot=false\n").unwrap();
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);
    let source = f.source();
    let global_held = Inventory::capture(&source).unwrap();
    assert!(global_held.last_commit_dates.is_empty());
    std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
    assert!(global_held.verify(&source).is_err());
    std::env::set_var("GIT_CONFIG_SYSTEM", &system);
    assert!(Inventory::capture(&source)
        .unwrap()
        .last_commit_dates
        .is_empty());
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    let isolated = Inventory::capture(&source).unwrap();
    assert_eq!(isolated.last_commit_dates["test_subject.py"], "2026-01-01");
    let alternate = f._root.path().join("alternate-index");
    fs::copy(f.git.join("index"), &alternate).unwrap();
    let mut index = git2::Index::open(&alternate).unwrap();
    index.clear().unwrap();
    index.write().unwrap();
    std::env::set_var("GIT_INDEX_FILE", &alternate);
    assert!(Inventory::capture(&source).unwrap().tracked.is_empty());
    assert!(isolated.verify(&source).is_err());
    std::env::remove_var("GIT_INDEX_FILE");
    let xdg = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("git");
    fs::create_dir(&xdg).unwrap();
    fs::write(xdg.join("config"), "[log]\nshowRoot=false\n").unwrap();
    assert!(Inventory::capture(&source)
        .err()
        .unwrap()
        .to_string()
        .contains("XDG"));
}
