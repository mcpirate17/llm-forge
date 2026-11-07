use super::*;
use std::{fs, process::Command};

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let f = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.email", "test@example.invalid"]);
        f.git(&["config", "user.name", "Test"]);
        f
    }
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.dir.path())
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn write(&self, path: &str, bytes: &str) {
        let path = self.dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn commit(&self) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "initial"]);
    }
    fn capture(&self) -> (tempfile::TempDir, Source, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        for path in self.oracle(&["ls-files", "-z"]) {
            let from = self.dir.path().join(&path);
            if !from.is_file() {
                continue;
            }
            let to = dir.path().join(&path);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(from, &to).unwrap();
            index.add_path(Path::new(&path)).unwrap();
        }
        let tree = index.write_tree_to(&repo).unwrap().to_string();
        let source = Source::open(dir.path()).unwrap();
        (dir, source, tree)
    }

    fn source(&self) -> Source {
        Source::open(self.dir.path()).unwrap()
    }
    fn oracle(&self, args: &[&str]) -> Vec<String> {
        let output = self.git(args);
        let mut paths: Vec<_> = output
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8(p.to_vec()).unwrap())
            .collect();
        paths.sort();
        paths
    }
}

#[test]
fn selection_matches_git_net_diff_and_tracked_inventory() {
    let f = Fixture::new();
    for path in [
        "reverted.py",
        "edited.py",
        "deleted.py",
        "renamed.py",
        "missing.py",
    ] {
        f.write(path, "original\n");
    }
    f.commit();
    f.write("reverted.py", "staged\n");
    f.write("edited.py", "staged\n");
    f.write("added.py", "added\n");
    f.git(&["add", "reverted.py", "edited.py", "added.py"]);
    f.write("reverted.py", "original\n");
    f.write("edited.py", "unstaged\n");
    fs::remove_file(f.dir.path().join("deleted.py")).unwrap();
    fs::remove_file(f.dir.path().join("missing.py")).unwrap();
    f.git(&["mv", "renamed.py", "new_name.py"]);
    f.write("untracked.py", "ignored\n");
    let source = f.source();
    let (_captured_dir, captured, tree) = f.capture();
    let selected = source
        .select_live_paths(
            LiveSelectionMode::ChangedFrom("HEAD".into()),
            &captured,
            &tree,
        )
        .unwrap();
    assert_eq!(
        selected.paths(),
        f.oracle(&[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=d",
            "--merge-base",
            "HEAD"
        ])
    );
    selected.verify(&source, &captured, &tree).unwrap();
    let all = source
        .select_live_paths(LiveSelectionMode::AllTracked, &captured, &tree)
        .unwrap();
    assert_eq!(all.paths(), f.oracle(&["ls-files", "-z"]));
    f.write("edited.py", "changed again\n");
    // Full bytes are sealed by the live snapshot binding, not this path-set binding.
    f.git(&["add", "edited.py"]);
    assert!(selected.verify(&source, &captured, &tree).is_err());
    assert!(all.verify(&source, &captured, &tree).is_err());
}

#[test]
fn moving_base_or_head_invalidates_selection() {
    let f = Fixture::new();
    f.write("a.py", "base\n");
    f.commit();
    f.git(&["branch", "base"]);
    let source = f.source();
    let (_captured_dir, captured, tree) = f.capture();
    let selected = source
        .select_live_paths(
            LiveSelectionMode::ChangedFrom("base".into()),
            &captured,
            &tree,
        )
        .unwrap();
    f.write("a.py", "next\n");
    f.commit();
    f.git(&["branch", "-f", "base", "HEAD"]);
    assert!(selected.verify(&source, &captured, &tree).is_err());
}

#[test]
fn content_transform_attributes_refuse_instead_of_guessing() {
    let f = Fixture::new();
    f.write(".gitattributes", "*.py text\n");
    f.write("a.py", "base\n");
    f.commit();
    f.write("a.py", "changed\n");
    let (_captured_dir, captured, tree) = f.capture();
    assert!(f
        .source()
        .select_live_paths(
            LiveSelectionMode::ChangedFrom("HEAD".into()),
            &captured,
            &tree
        )
        .is_err());
}

#[test]
fn alternate_index_is_bound_without_mutation() {
    if let Some(root) = std::env::var_os("FORGE_SELECTION_CHILD_ROOT") {
        let source = Source::open(Path::new(&root)).unwrap();
        let captured = Source::open(Path::new(
            &std::env::var_os("FORGE_SELECTION_CAPTURE").unwrap(),
        ))
        .unwrap();
        let tree = std::env::var("FORGE_SELECTION_TREE").unwrap();
        let index_path = PathBuf::from(std::env::var_os("GIT_INDEX_FILE").unwrap());
        let before = fs::read(&index_path).unwrap();
        let selection = source
            .select_live_paths(
                LiveSelectionMode::ChangedFrom("HEAD".into()),
                &captured,
                &tree,
            )
            .unwrap();
        assert_eq!(selection.paths(), ["added.py"]);
        assert_eq!(fs::read(&index_path).unwrap(), before);
        fs::write(&index_path, b"changed").unwrap();
        assert!(selection.verify(&source, &captured, &tree).is_err());
        return;
    }
    let f = Fixture::new();
    f.write("base.py", "base\n");
    f.commit();
    f.write("added.py", "added\n");
    f.git(&["add", "added.py"]);
    let (capture_dir, _captured, tree) = f.capture();
    let alternate = f.dir.path().join(".git/alternate-index");
    fs::copy(f.dir.path().join(".git/index"), &alternate).unwrap();
    f.git(&["reset", "-q", "HEAD", "--", "added.py"]);
    let oracle = Command::new("git")
        .args([
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=d",
            "--merge-base",
            "HEAD",
        ])
        .current_dir(f.dir.path())
        .env("GIT_INDEX_FILE", &alternate)
        .output()
        .unwrap();
    assert!(oracle.status.success());
    assert_eq!(oracle.stdout, b"added.py\0");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            std::thread::current().name().unwrap(),
            "--nocapture",
        ])
        .env("FORGE_SELECTION_CHILD_ROOT", f.dir.path())
        .env("FORGE_SELECTION_CAPTURE", capture_dir.path())
        .env("FORGE_SELECTION_TREE", tree)
        .env("GIT_INDEX_FILE", alternate)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn captured_tree_selection_does_not_read_live_file_contents() {
    let f = Fixture::new();
    f.write("large.bin", "original\n");
    f.commit();
    f.write("large.bin", "captured change\n");
    let (_dir, captured, tree) = f.capture();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(f.dir.path().join("large.bin"))
        .unwrap();
    file.set_len(128 * 1024 * 1024).unwrap();
    let source = f.source();
    let selection = source
        .select_live_paths(
            LiveSelectionMode::ChangedFrom("HEAD".into()),
            &captured,
            &tree,
        )
        .unwrap();
    assert_eq!(selection.paths(), ["large.bin"]);
    selection.verify(&source, &captured, &tree).unwrap();
}

#[test]
fn duplicate_paths_in_a_raw_tree_are_refused() {
    let f = Fixture::new();
    let repo = git2::Repository::open(f.dir.path()).unwrap();
    let odb = repo.odb().unwrap();
    let first = odb.write(git2::ObjectType::Blob, b"first\n").unwrap();
    let second = odb.write(git2::ObjectType::Blob, b"second\n").unwrap();
    let mut raw = Vec::new();
    for oid in [first, second] {
        raw.extend_from_slice(b"100644 duplicate.py\0");
        raw.extend_from_slice(oid.as_bytes());
    }
    let tree = odb.write(git2::ObjectType::Tree, &raw).unwrap();
    let source = f.source();
    assert!(entries(&source, &tree.to_string()).is_err());
}
