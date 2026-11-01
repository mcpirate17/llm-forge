use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let snap = temp.path().join("snapshot");
    let tmp = temp.path().join("private");
    fs::create_dir(&snap).unwrap();
    fs::create_dir(&tmp).unwrap();
    fs::set_permissions(&snap, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, snap, tmp)
}

#[test]
fn writes_private_case_once_and_removes_owned_root() {
    let (_temp, snap, tmp) = fixture();
    let mut state = State::new_in(&snap, &tmp).unwrap();
    let path = state.path();
    let (db, config) = state.write_case(2, b"[]", b"Checks: '*'\n").unwrap();
    assert_eq!(db.file_name().unwrap(), "00002");
    assert_eq!(fs::read(db.join("compile_commands.json")).unwrap(), b"[]");
    assert_eq!(fs::read(&config).unwrap(), b"Checks: '*'\n");
    assert_eq!(fs::metadata(&db).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(db.join("compile_commands.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&config).unwrap().mode() & 0o777, 0o600);
    assert!(state.write_case(2, b"other", b"x").is_err());
    state.finish().unwrap();
    assert!(!path.exists());
}

#[test]
fn invalid_limits_and_scratch_inside_snapshot_refuse() {
    let (_temp, snap, tmp) = fixture();
    let nested = snap.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(State::new_in(&snap, &nested).is_err());
    let mut state = State::new_in(&snap, &tmp).unwrap();
    assert!(state.write_case(MAX_CASES, b"[]", b"x").is_err());
    assert!(state.write_case(0, &vec![0; MAX_DB + 1], b"x").is_err());
    assert!(state
        .write_case(0, b"[]", &vec![0; MAX_CONFIG + 1])
        .is_err());
    state.bytes = MAX_TOTAL;
    assert!(state.write_case(0, b"a", b"").is_err());
    assert!(!state.path().join("00000").exists());
    state.finish().unwrap();
}

#[test]
fn replaced_state_and_parent_are_never_deleted() {
    let (temp, snap, tmp) = fixture();
    let state = State::new_in(&snap, &tmp).unwrap();
    let path = state.path();
    let old = tmp.join("old-state");
    fs::rename(&path, &old).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("sentinel"), b"keep").unwrap();
    assert!(state.finish().is_err());
    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"keep");
    let state = State::new_in(&snap, &tmp).unwrap();
    fs::rename(&tmp, temp.path().join("old-private")).unwrap();
    fs::create_dir(&tmp).unwrap();
    assert!(state.finish().is_err());
}

#[test]
fn failed_created_directory_admission_reports_path_and_preserves_entry() {
    let (_temp, _snap, tmp) = fixture();
    let path = tmp.join("created-entry");
    fs::write(&path, b"sentinel").unwrap();
    let parent = Dir::open_ambient_dir(&tmp, cap_std::ambient_authority()).unwrap();
    let error = match open_created(&parent, &tmp, "created-entry") {
        Ok(_) => panic!("non-directory unexpectedly accepted"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains(path.to_str().unwrap()));
    assert_eq!(fs::read(path).unwrap(), b"sentinel");
}
