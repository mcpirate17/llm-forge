use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    _tmp: tempfile::TempDir,
    snapshot: PathBuf,
    first: PathBuf,
    second: PathBuf,
}

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, data: &[u8]) {
    fs::write(path, data).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let snapshot = tmp.path().join("snapshot");
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        for dir in [&snapshot, &first, &second] {
            private_dir(dir);
        }
        Self {
            _tmp: tmp,
            snapshot,
            first,
            second,
        }
    }
    fn metadata(&self, root: &Path, label: &str, text: &[u8]) -> PathBuf {
        let directory = root.join(format!("{label}.dist-info"));
        private_dir(&directory);
        let path = directory.join("METADATA");
        private_file(&path, text);
        path
    }
    fn capture(&self, roots: &[PathBuf], names: &[&str]) -> Result<Installed> {
        Installed::capture(
            roots,
            names.iter().map(|s| (*s).into()).collect(),
            &self.snapshot,
        )
    }
}

#[test]
fn metadata_headers_normalize_and_reject_ambiguity() {
    assert_eq!(
        headers(b"Metadata-Version: 2.3\nName: My_Pkg\nVersion: 1.2.3\n\nbody\n").unwrap(),
        ("my-pkg".into(), "1.2.3".into())
    );
    for invalid in [
        b"Name: x\nName: y\nVersion: 1\n".as_slice(),
        b"Name: x\nVersion: 1\nversion: 2\n",
        b"Name: x\n y\nVersion: 1\n",
        b"Name: x\nVersion: 1\n y\n",
        b"Name: \xff\nVersion: 1\n",
        b"Name: x\nVersion: \n",
        b"no-colon\n",
    ] {
        assert!(headers(invalid).is_err());
    }
    assert!(headers(b"Name: x\nSummary: a\n continued\nVersion: 1\n").is_ok());
}

#[test]
fn ordered_roots_first_match_wins_and_empty_is_explicit() {
    let f = Fixture::new();
    f.metadata(&f.first, "my_pkg-1.0", b"Name: My-Pkg\nVersion: 1.0\n");
    f.metadata(&f.second, "my_pkg-2.0", b"Name: my_pkg\nVersion: 2.0\n");
    let roots = vec![f.first.clone(), f.second.clone()];
    let result = f.capture(&roots, &["my-pkg", "missing"]).unwrap();
    assert_eq!(
        result.packages,
        vec![json!({"name":"my-pkg","version":"1.0"})]
    );
    assert_eq!(result.evidence.len(), 1);
    result.verify().unwrap();
    assert!(f.capture(&[], &["my-pkg"]).unwrap().packages.is_empty());
    assert!(f
        .capture(&[f.first.clone(), f.first.clone()], &["my-pkg"])
        .is_err());
}

#[test]
fn unrelated_metadata_is_not_parsed_and_duplicates_refuse() {
    let f = Fixture::new();
    f.metadata(&f.first, "other-1.0", b"\xff bad unrelated metadata");
    f.metadata(&f.first, "alpha-1.0", b"Name: Alpha\nVersion: 1.0\n");
    assert_eq!(
        f.capture(std::slice::from_ref(&f.first), &["alpha"])
            .unwrap()
            .packages
            .len(),
        1
    );
    f.metadata(&f.first, "alpha-2.0", b"Name: alpha\nVersion: 2.0\n");
    assert!(f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .is_err());
}

#[test]
fn prefix_collision_does_not_parse_another_distribution() {
    let f = Fixture::new();
    f.metadata(&f.first, "foo-bar-1.0", b"\xff malformed unrelated");
    assert!(f
        .capture(std::slice::from_ref(&f.first), &["foo"])
        .unwrap()
        .packages
        .is_empty());
}

#[test]
fn selected_label_and_metadata_name_must_agree() {
    let f = Fixture::new();
    f.metadata(&f.first, "foo-1.0", b"Name: bar\nVersion: 1.0\n");
    assert!(f.capture(std::slice::from_ref(&f.first), &["foo"]).is_err());
}

#[test]
fn held_file_and_new_selected_entry_drift_are_detected() {
    let f = Fixture::new();
    let file = f.metadata(&f.first, "alpha-1.0", b"Name: alpha\nVersion: 1.0\n");
    let bound = f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .unwrap();
    private_file(&file, b"Name: alpha\nVersion: 1.1\n");
    assert!(bound.verify().is_err());
    let bound = f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .unwrap();
    f.metadata(&f.first, "alpha-2.0", b"Name: alpha\nVersion: 2.0\n");
    assert!(bound.verify().is_err());
}

#[test]
fn symlink_roots_metadata_directories_and_files_refuse() {
    let f = Fixture::new();
    let root_alias = f._tmp.path().join("root-alias");
    symlink(&f.first, &root_alias).unwrap();
    assert!(f.capture(&[root_alias], &["alpha"]).is_err());
    let dir = f.first.join("alpha-1.0.dist-info");
    symlink(&f.second, &dir).unwrap();
    assert!(f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .is_err());
    fs::remove_file(&dir).unwrap();
    private_dir(&dir);
    let target = f.second.join("target");
    private_file(&target, b"Name: alpha\nVersion: 1.0\n");
    symlink(&target, dir.join("METADATA")).unwrap();
    assert!(f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .is_err());
    assert!(f
        .capture(std::slice::from_ref(&f.snapshot), &["alpha"])
        .is_err());
}

#[test]
fn root_count_and_metadata_size_bounds_refuse() {
    let f = Fixture::new();
    assert!(f.capture(&vec![f.first.clone(); 33], &["alpha"]).is_err());
    f.metadata(&f.first, "alpha-1.0", &vec![b'x'; MAX_METADATA + 1]);
    assert!(f
        .capture(std::slice::from_ref(&f.first), &["alpha"])
        .is_err());
}
