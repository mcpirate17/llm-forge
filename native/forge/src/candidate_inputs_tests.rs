use super::*;
use std::{
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    },
};

fn mkdir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn blob(bytes: &[u8]) -> String {
    Oid::hash_object(ObjectType::Blob, bytes)
        .unwrap()
        .to_string()
}

#[test]
fn external_pin_checks_real_path_digest_and_held_identity() {
    let temp = tempfile::tempdir().unwrap();
    let snap = temp.path().join("snapshot");
    let operator = temp.path().join("operator");
    mkdir(&snap).unwrap();
    mkdir(&operator).unwrap();
    let first = operator.join("first.so");
    let second = operator.join("second.so");
    write(&first, b"same").unwrap();
    write(&second, b"same").unwrap();
    let (bound, _) = external_with_tmp(&first, &sha(b"same"), 1024, &snap, None).unwrap();
    bound.verify_external_pin(&first, &sha(b"same")).unwrap();
    assert!(bound.verify_external_pin(&second, &sha(b"same")).is_err());
    assert!(bound.verify_external_pin(&first, &sha(b"other")).is_err());
    fs::remove_file(&first).unwrap();
    write(&first, b"same").unwrap();
    assert!(bound.verify_external_pin(&first, &sha(b"same")).is_err());
}

#[test]
fn pinned_external_digest_and_parent_drift() {
    let temp = tempfile::tempdir().unwrap();
    let snap = temp.path().join("snapshot");
    let operator = temp.path().join("operator");
    mkdir(&snap).unwrap();
    mkdir(&operator).unwrap();
    let path = operator.join("compile_commands.json");
    write(&path, b"[]").unwrap();
    let (bound, bytes) = external_with_tmp(&path, &sha(b"[]"), 1024, &snap, None).unwrap();
    assert_eq!(bytes, b"[]");
    bound.verify().unwrap();
    assert!(external_with_tmp(&path, &sha(b"wrong"), 1024, &snap, None).is_err());
    write(&path, b"{}\n").unwrap();
    assert!(bound.verify().is_err());
    let (bound, _) = external_with_tmp(&path, &sha(b"{}\n"), 1024, &snap, None).unwrap();
    fs::rename(&operator, temp.path().join("old-operator")).unwrap();
    mkdir(&operator).unwrap();
    assert!(bound.verify().is_err());
}

#[test]
fn snapshot_blob_and_untrusted_paths() {
    let temp = tempfile::tempdir().unwrap();
    let snap = temp.path().join("snapshot");
    mkdir(&snap).unwrap();
    fs::write(snap.join("db.json"), b"[]").unwrap();
    let (bound, _) = snapshot(&snap, "db.json", &blob(b"[]"), 1024).unwrap();
    bound.verify().unwrap();
    assert!(snapshot(&snap, "../db.json", &blob(b"[]"), 1024).is_err());
    assert!(snapshot(&snap, "db.json", &blob(b"wrong"), 1024).is_err());
    assert!(external_with_tmp(&snap.join("db.json"), &sha(b"[]"), 1024, &snap, None).is_err());
    fs::write(snap.join("db.json"), b"{}\n").unwrap();
    assert!(bound.verify().is_err());
}

#[test]
fn aliases_fifo_hardlinks_and_scratch_excluded() {
    let temp = tempfile::tempdir().unwrap();
    let snap = temp.path().join("snapshot");
    let dir = temp.path().join("operator");
    mkdir(&snap).unwrap();
    mkdir(&dir).unwrap();
    let file = dir.join("db");
    write(&file, b"[]").unwrap();
    symlink(&file, dir.join("alias")).unwrap();
    assert!(external_with_tmp(&dir.join("alias"), &sha(b"[]"), 1024, &snap, None).is_err());
    fs::hard_link(&file, dir.join("hardlink")).unwrap();
    assert!(external_with_tmp(&file, &sha(b"[]"), 1024, &snap, None).is_err());
    fs::remove_file(dir.join("hardlink")).unwrap();
    assert!(external_with_tmp(&file, &sha(b"[]"), 1024, &snap, Some(&dir)).is_err());
    let fifo = dir.join("fifo");
    let cpath = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(external_with_tmp(&fifo, &sha(b"[]"), 1024, &snap, None).is_err());
}

#[test]
fn directory_binding_detects_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("directory");
    mkdir(&path).unwrap();
    let bound = DirectoryBinding::open(&path, None).unwrap();
    bound.verify().unwrap();
    fs::rename(&path, temp.path().join("old")).unwrap();
    mkdir(&path).unwrap();
    assert!(bound.verify().is_err());
}

#[test]
fn repository_group_write_and_replaced_ancestor_remain_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("repo");
    let directory = parent.join("dir");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o775)).unwrap();
    let path = directory.join("data");
    fs::write(&path, b"unchanged").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(repository_live(&path, 1024).is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o775)).unwrap();
    let (held, _) = repository_live(&path, 1024).unwrap();
    held.verify().unwrap();
    assert!(DirectoryBinding::open(&directory, None).is_err());
    fs::rename(&directory, parent.join("old")).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(&path, b"unchanged").unwrap();
    assert!(held.verify().is_err());
}
