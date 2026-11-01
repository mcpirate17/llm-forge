use super::*;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture {
    root: PathBuf,
    host: PathBuf,
    config: PathBuf,
    scratch: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-candidate-workspace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let host = root.join("host");
        fs::create_dir(&host).unwrap();
        let repo = git2::Repository::init(&host).unwrap();
        repo.index().unwrap().write().unwrap();
        let scratch = root.join("scratch");
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("grants.json");
        let fixture = Self {
            root,
            host,
            config,
            scratch,
        };
        fixture.write_config(fixture.value());
        fixture
    }
    fn value(&self) -> Value {
        json!({"version":1,"source_root":self.host,"read_only":[self.host,"/usr/bin"],"programs":{"true":"/usr/bin/true"},"environment":{},"scratch_parent":self.scratch,"cgroup_parent":"/sys/fs/cgroup","resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},"max_wall_seconds":60,"max_cpu_seconds":60,"max_address_space_mib":1024,"python":null})
    }
    fn write_config(&self, value: Value) {
        fs::write(&self.config, value.to_string()).unwrap();
        fs::set_permissions(&self.config, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn source(&self) -> Source {
        Source::open(&self.host).unwrap()
    }
    fn tree(&self) -> String {
        git2::Repository::open(&self.host)
            .unwrap()
            .treebuilder(None)
            .unwrap()
            .write()
            .unwrap()
            .to_string()
    }
    fn grants(&self) -> Grants {
        Grants::load(&self.config, &self.source()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn grants_reject_unsafe_or_untrusted_configuration() {
    let f = Fixture::new();
    let source = f.source();
    assert_eq!(f.grants().digest().len(), 64);
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o660)).unwrap();
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["version"] = json!(2);
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["unexpected"] = json!(true);
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    let mut value = f.value();
    value["environment"] = json!({"GIT_DIR":"/tmp"});
    f.write_config(value);
    assert!(Grants::load(&f.config, &source).is_err());
    fs::remove_file(&f.config).unwrap();
    symlink(f.host.join("missing"), &f.config).unwrap();
    assert!(Grants::load(&f.config, &source).is_err());
}
#[test]
fn private_git_is_detached_and_source_index_untouched() {
    let f = Fixture::new();
    let source = f.source();
    let tree = f.tree();
    let grants = f.grants();
    let before = fs::read(f.host.join(".git/index")).unwrap();
    let before_head = fs::read(f.host.join(".git/HEAD")).unwrap();
    let workspace = Workspace::new(&source, &tree, &grants).unwrap();
    let repo = git2::Repository::open_bare(&workspace.git_dir).unwrap();
    assert!(repo.head_detached().unwrap());
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree_id()
            .to_string(),
        tree
    );
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), before);
    assert_eq!(fs::read(f.host.join(".git/HEAD")).unwrap(), before_head);
    assert!(repo.find_reference("refs/heads/master").is_err());
    drop(repo);
    let path = workspace.scratch.path.clone();
    workspace.discard().unwrap();
    assert!(!path.exists());
}
#[test]
fn armed_workspace_is_retained_after_unsafe_cleanup() {
    let f = Fixture::new();
    let source = f.source();
    let mut workspace = Workspace::new(&source, &f.tree(), &f.grants()).unwrap();
    let scratch = workspace.scratch.path.clone();
    let snapshot = workspace.path();
    Resources::arm(&mut workspace);
    assert!(Box::new(workspace).finish(false).is_err());
    assert!(scratch.is_dir() && snapshot.is_dir());
    fs::remove_dir_all(scratch).unwrap();
}
#[test]
fn changed_file_artifact_rejects_escape() {
    let f = Fixture::new();
    let source = f.source();
    let grants = f.grants();
    let workspace = Workspace::new(&source, &f.tree(), &grants).unwrap();
    let bad = PreparedArgv {
        argv: vec!["true".into()],
        changed_files: Some(crate::candidate_command::ChangedFiles {
            path: workspace.runtime().join("../escape"),
            bytes: b"x".to_vec(),
        }),
    };
    assert!(workspace.plan(&bad, &grants).is_err());
    assert!(!workspace.scratch.path.join("escape").exists());
    workspace.discard().unwrap();
}
