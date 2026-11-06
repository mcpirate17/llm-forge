use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().join("guest");
    for directory in [
        "bin",
        "lib/python3.12/lib-dynload",
        "lib/python3.12/dist-packages/pytest",
        "lib/python3.12/dist-packages/_pytest",
        "lib/python3.12/dist-packages/pytest_cov",
        "lib/python3.12/dist-packages/coverage",
        "pkg",
        "pkg-version",
    ] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    for entry in walk_dirs(&root) {
        fs::set_permissions(entry, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for file in [
        "lib/python3.12/dist-packages/pytest/__init__.py",
        "lib/python3.12/dist-packages/pytest/__main__.py",
        "lib/python3.12/dist-packages/_pytest/__init__.py",
        "lib/python3.12/dist-packages/pytest_cov/__init__.py",
        "lib/python3.12/dist-packages/pytest_cov/plugin.py",
        "lib/python3.12/dist-packages/coverage/__init__.py",
        "pkg/child",
        "pkg-version/child",
    ] {
        let path = root.join(file);
        fs::write(&path, b"fixture").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    temp
}
fn walk_dirs(path: &Path) -> Vec<PathBuf> {
    let mut result = vec![path.to_owned()];
    for item in fs::read_dir(path).unwrap() {
        let path = item.unwrap().path();
        if path.is_dir() {
            result.extend(walk_dirs(&path));
        }
    }
    result
}

#[test]
fn export_sorts_globally_and_roundtrips_held_admission_schema() {
    let f = fixture();
    let root = f.path().join("guest");
    let output = f.path().join("manifest.json");
    let exported = export(&root, &output).unwrap();
    let bytes = fs::read(&output).unwrap();
    use sha2::{Digest as _, Sha256};
    assert_eq!(
        exported.manifest_sha256,
        format!("{:x}", Sha256::digest(&bytes))
    );
    let manifest: GuestManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest.schema_version, 1);
    validate_entries(&manifest.entries).unwrap();
    validate_layout(&manifest.entries).unwrap();
    let names: Vec<_> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        names.iter().position(|p| *p == "pkg-version")
            < names.iter().position(|p| *p == "pkg/child")
    );
}

#[test]
fn export_refuses_overlap_clobber_symlink_and_unsafe_parent() {
    let f = fixture();
    let root = f.path().join("guest");
    assert!(export(&root, &root.join("manifest.json")).is_err());
    let existing = f.path().join("existing");
    fs::write(&existing, b"preserve").unwrap();
    assert!(export(&root, &existing).is_err());
    assert_eq!(fs::read(&existing).unwrap(), b"preserve");
    let alias = f.path().join("alias");
    symlink(f.path(), &alias).unwrap();
    assert!(export(&root, &alias.join("new")).is_err());
    let unsafe_parent = f.path().join("unsafe");
    fs::create_dir(&unsafe_parent).unwrap();
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(export(&root, &unsafe_parent.join("new")).is_err());
}

#[test]
fn export_refuses_missing_layout_and_oversize_before_output() {
    let f = fixture();
    let root = f.path().join("guest");
    let output = f.path().join("manifest.json");
    fs::remove_file(root.join("lib/python3.12/dist-packages/pytest/__init__.py")).unwrap();
    assert!(format!("{:#}", export(&root, &output).err().unwrap())
        .contains("required package module missing"));
    assert!(!output.exists());
    fs::write(
        root.join("lib/python3.12/dist-packages/pytest/__init__.py"),
        b"fixture",
    )
    .unwrap();
    fs::set_permissions(
        root.join("lib/python3.12/dist-packages/pytest/__init__.py"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let oversized = root.join("oversized");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    fs::set_permissions(oversized, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(format!("{:#}", export(&root, &output).err().unwrap())
        .contains("seed file must be bounded"));
    assert!(!output.exists());
}

#[test]
fn held_export_refuses_post_write_content_drift_or_named_replacement() {
    for replace in [false, true] {
        let f = fixture();
        let parent = private_directory(f.path()).unwrap();
        let name = std::ffi::OsStr::new("manifest.json");
        let mut file = write_export(&parent, name, b"original").unwrap();
        let path = f.path().join(name);
        if replace {
            fs::remove_file(&path).unwrap();
        }
        fs::write(&path, b"changed").unwrap();
        assert!(verify_export(&parent, name, &mut file, b"original").is_err());
    }
}
