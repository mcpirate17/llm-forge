use super::*;
use std::os::unix::fs::symlink;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-dead-inputs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn write(root: &Path, name: &str, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
#[test]
fn capture_detects_source_content_and_same_bytes_inode_drift() {
    let root = Temp::new();
    write(root.path(), "pkg/source.py", b"VALUE = 1\n");
    let mut held = Capture::open(root.path()).unwrap();
    assert_eq!(held.read_bytes("pkg/source.py").unwrap(), b"VALUE = 1\n");
    assert_eq!(held.sources().next().unwrap().2, 10);
    held.verify().unwrap();
    write(root.path(), "pkg/source.py", b"VALUE = 2\n");
    assert!(held.read_bytes("pkg/source.py").is_err());
    assert!(held.verify().is_err());
    write(root.path(), "pkg/source.py", b"VALUE = 1\n");
    let mut held = Capture::open(root.path()).unwrap();
    held.read_bytes("pkg/source.py").unwrap();
    fs::rename(
        root.path().join("pkg/source.py"),
        root.path().join("pkg/old.py"),
    )
    .unwrap();
    write(root.path(), "pkg/source.py", b"VALUE = 1\n");
    assert!(held.verify().is_err());
}
#[test]
fn positive_and_negative_lookups_are_replayed() {
    let root = Temp::new();
    write(root.path(), "pkg/present.py", b"");
    let mut held = Capture::open(root.path()).unwrap();
    assert!(held.is_file("pkg/present.py").unwrap());
    assert!(!held.is_file("pkg/new/nested.py").unwrap());
    assert_eq!(held.lookups().get("pkg/new/nested.py"), Some(&false));
    held.verify().unwrap();
    write(root.path(), "pkg/new/nested.py", b"");
    assert!(held.verify().is_err());
    fs::remove_file(root.path().join("pkg/new/nested.py")).unwrap();
    fs::remove_file(root.path().join("pkg/present.py")).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn rejects_symlinks_escapes_nonfiles_and_oversize() {
    let root = Temp::new();
    write(root.path(), "actual/input.py", b"ok");
    symlink(root.path().join("actual"), root.path().join("alias")).unwrap();
    symlink(
        root.path().join("actual/input.py"),
        root.path().join("link.py"),
    )
    .unwrap();
    let mut held = Capture::open(root.path()).unwrap();
    for path in ["../outside", "alias/input.py", "link.py", "actual"] {
        assert!(held.read_bytes(path).is_err(), "{path}");
    }
    assert!(held.is_file("alias/missing.py").is_err());
    assert!(held.is_file("link.py").is_err());
    let large = fs::File::create(root.path().join("large.py")).unwrap();
    large.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(held.read_bytes("large.py").is_err());
}
#[test]
fn named_root_replacement_is_not_accepted() {
    let base = Temp::new();
    let root = base.path().join("root");
    fs::create_dir(&root).unwrap();
    let held = Capture::open(&root).unwrap();
    held.verify().unwrap();
    fs::rename(&root, base.path().join("old")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn report_input_accepts_lossy_bytes_but_strict_scanner_does_not() {
    let root = Temp::new();
    write(root.path(), "pkg/source.py", b"# \xff\nVALUE = 1\n");
    let tracked = vec!["pkg/source.py".to_owned()];
    let mut held = Capture::open(root.path()).unwrap();
    let analysis =
        super::super::report::Analysis::from_captured(root.path(), &tracked, &mut held).unwrap();
    assert_eq!(
        analysis
            .report("", "", &BTreeMap::new())
            .unwrap()
            .modules_scanned,
        1
    );
    held.verify().unwrap();
    assert!(super::super::bounded::scan(root.path(), &tracked).is_err());
}
