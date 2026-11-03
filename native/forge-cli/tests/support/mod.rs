use git2::{Oid, Repository, Signature};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Fixture {
    base: PathBuf,
    pub root: PathBuf,
    pub recovery: PathBuf,
    pub old: Oid,
    pub new: Oid,
}
impl Fixture {
    pub fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "forge-checkout-cli-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
        let root = base.join("repo");
        let recovery = base.join("recovery");
        fs::DirBuilder::new().mode(0o700).create(&recovery).unwrap();
        fs::create_dir(base.join("home")).unwrap();
        let (old, new) = initialize(&root);
        Self {
            base,
            root,
            recovery,
            old,
            new,
        }
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_")
                || key.to_string_lossy().starts_with("LLM_GOVERNANCE_")
            {
                command.env_remove(key);
            }
        }
        command.env("HOME", self.base.join("home"));
        command.env("XDG_CONFIG_HOME", self.base.join("home"));
        command.arg("checkout-sync");
        command
    }
    pub fn run(&self, arguments: &[&str]) -> Output {
        self.command()
            .arg("--repo")
            .arg(&self.root)
            .args(arguments)
            .output()
            .unwrap()
    }
    pub fn execute(&self) -> Output {
        self.command()
            .arg("--repo")
            .arg(&self.root)
            .arg("--recovery-dir")
            .arg(&self.recovery)
            .arg("--json")
            .output()
            .unwrap()
    }
    pub fn stage_layers(&self) {
        fs::write(self.root.join("a"), b"ONE\ntwo\nthree\nfour\nfive\n").unwrap();
        let repo = Repository::open(&self.root).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a")).unwrap();
        index.write().unwrap();
        fs::write(self.root.join("a"), b"ONE\ntwo\nthree\nfour\nFIVE\n").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}
fn initialize(root: &Path) -> (Oid, Oid) {
    let repo = Repository::init(root).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    fs::write(root.join("a"), b"one\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(root.join("b"), b"old\n").unwrap();
    let mut index = repo.index().unwrap();
    for path in ["a", "b"] {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let old = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let mut builder = repo.treebuilder(Some(&tree)).unwrap();
    builder
        .insert(
            "a",
            repo.blob(b"one\ntwo\nTHREE\nfour\nfive\n").unwrap(),
            0o100644,
        )
        .unwrap();
    builder
        .insert("b", repo.blob(b"new\n").unwrap(), 0o100644)
        .unwrap();
    let next = repo.find_tree(builder.write().unwrap()).unwrap();
    let parent = repo.find_commit(old).unwrap();
    let new = repo
        .commit(None, &sig, &sig, "incoming", &next, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", new, false, "fixture")
        .unwrap();
    (old, new)
}
pub fn json(output: &Output, expected: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    mode: u32,
    inode: u64,
    modified: (i64, i64),
    bytes: Vec<u8>,
}
pub fn image(root: &Path) -> BTreeMap<PathBuf, Image> {
    fn walk(root: &Path, relative: &Path, out: &mut BTreeMap<PathBuf, Image>) {
        for item in fs::read_dir(root.join(relative)).unwrap() {
            let item = item.unwrap();
            let path = relative.join(item.file_name());
            // This is the documented, sole dry-run persistence exception.
            if path == Path::new(".git/governance/commit-review.lock") {
                continue;
            }
            let metadata = fs::symlink_metadata(item.path()).unwrap();
            if metadata.is_dir() {
                if path != Path::new(".git/governance") {
                    out.insert(
                        path.clone(),
                        Image {
                            mode: metadata.mode(),
                            inode: metadata.ino(),
                            modified: (0, 0),
                            bytes: Vec::new(),
                        },
                    );
                }
                walk(root, &path, out);
            } else {
                let bytes = if metadata.file_type().is_symlink() {
                    use std::os::unix::ffi::OsStrExt;
                    fs::read_link(item.path())
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec()
                } else {
                    fs::read(item.path()).unwrap()
                };
                out.insert(
                    path,
                    Image {
                        mode: metadata.mode(),
                        inode: metadata.ino(),
                        modified: (metadata.mtime(), metadata.mtime_nsec()),
                        bytes,
                    },
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}
