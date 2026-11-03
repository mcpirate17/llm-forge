//! Cached directory bindings must retain the original no-follow admission checks.
use super::super::fixture::Fixture;
use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt as _},
};

fn nested(fixture: &Fixture) -> PathBuf {
    let venv = fixture.root.join("outer/inner/venv");
    fs::create_dir_all(venv.join("bin")).unwrap();
    fs::write(venv.join("bin/sentinel"), b"held original bytes").unwrap();
    venv
}
fn changed(bin: &Bin) {
    let error = bin.verify().unwrap_err();
    assert!(
        format!("{error:#}").contains("identity changed"),
        "{error:#}"
    );
    assert_eq!(bin.dir.read("sentinel").unwrap(), b"held original bytes");
}
#[test]
fn normalized_parent_walk_keeps_initial_no_follow_and_existence_checks() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("checked")).unwrap();
    let selected = fixture.root.join("checked/../source-venv");
    let bin = Bin::open(&selected).unwrap();
    assert_eq!(bin.path, fixture.source.join("bin"));
    bin.verify().unwrap();
    fs::remove_dir(fixture.root.join("checked")).unwrap();
    // Normalization must not erase initial traversal through missing/file/link entries.
    assert!(Bin::open(&selected).is_err());
    fs::write(fixture.root.join("checked"), b"not a directory").unwrap();
    assert!(Bin::open(&selected).is_err());
    fs::remove_file(fixture.root.join("checked")).unwrap();
    symlink(&fixture.destination, fixture.root.join("checked")).unwrap();
    assert!(Bin::open(&selected).is_err());
    // A discarded component is not part of the already-normalized selected binding.
    bin.verify().unwrap();
    assert!(Bin::open(Path::new("/../../")).is_err());
    assert!(Bin::open(Path::new("")).is_err());
}
#[test]
fn changed_ancestor_is_detected_even_if_final_venv_and_bin_inodes_are_preserved() {
    let fixture = Fixture::new();
    let venv = nested(&fixture);
    let bin = Bin::open(&venv).unwrap();
    let root_inode = fs::metadata(&venv).unwrap().ino();
    let bin_inode = fs::metadata(&bin.path).unwrap().ino();
    let outer = fixture.root.join("outer");
    let saved = fixture.root.join("saved-outer");
    fs::rename(&outer, &saved).unwrap();
    fs::create_dir(&outer).unwrap();
    fs::rename(saved.join("inner"), outer.join("inner")).unwrap();
    assert_eq!(fs::metadata(&venv).unwrap().ino(), root_inode);
    assert_eq!(fs::metadata(&bin.path).unwrap().ino(), bin_inode);
    changed(&bin);
}
#[test]
fn ancestor_symlink_to_the_original_tree_is_never_followed_by_recheck() {
    let fixture = Fixture::new();
    let venv = nested(&fixture);
    let bin = Bin::open(&venv).unwrap();
    let outer = fixture.root.join("outer");
    let saved = fixture.root.join("saved-outer");
    fs::rename(&outer, &saved).unwrap();
    symlink(&saved, &outer).unwrap();
    assert_eq!(fs::metadata(&bin.path).unwrap().ino(), bin.identity().1);
    changed(&bin);
}
#[test]
fn virtualenv_replacement_and_symlink_alias_leave_the_held_bin_bound_to_original() {
    for link in [false, true] {
        let fixture = Fixture::new();
        let venv = nested(&fixture);
        let bin = Bin::open(&venv).unwrap();
        let saved = venv.with_file_name("saved-venv");
        fs::rename(&venv, &saved).unwrap();
        if link {
            symlink(&saved, &venv).unwrap();
        } else {
            fs::create_dir_all(venv.join("bin")).unwrap();
            fs::write(venv.join("bin/sentinel"), b"foreign root bytes").unwrap();
        }
        changed(&bin);
        assert_eq!(
            fs::read(saved.join("bin/sentinel")).unwrap(),
            b"held original bytes"
        );
        if !link {
            assert_eq!(
                fs::read(venv.join("bin/sentinel")).unwrap(),
                b"foreign root bytes"
            );
        }
    }
}
#[test]
fn bin_replacement_and_symlink_alias_never_rebind_the_held_descriptor() {
    for link in [false, true] {
        let fixture = Fixture::new();
        let venv = nested(&fixture);
        let bin = Bin::open(&venv).unwrap();
        let saved = venv.join("saved-bin");
        fs::rename(&bin.path, &saved).unwrap();
        if link {
            symlink(&saved, &bin.path).unwrap();
        } else {
            fs::create_dir(&bin.path).unwrap();
            fs::write(bin.path.join("sentinel"), b"foreign bin bytes").unwrap();
        }
        changed(&bin);
        assert_eq!(
            fs::read(saved.join("sentinel")).unwrap(),
            b"held original bytes"
        );
        if !link {
            assert_eq!(
                fs::read(bin.path.join("sentinel")).unwrap(),
                b"foreign bin bytes"
            );
        }
    }
}
// Filter by this fixture's unique path: concurrent tests may open unrelated FDs.
fn descriptors_under(root: &Path) -> BTreeSet<OsString> {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            match fs::read_link(entry.path()) {
                Ok(target) if target.starts_with(root) => Some(entry.file_name()),
                Ok(_) => None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("inspect test descriptor: {error}"),
            }
        })
        .collect()
}
#[test]
fn directory_descriptors_are_cloexec_stable_across_checks_and_released_on_return() {
    let fixture = Fixture::new();
    let venv = nested(&fixture);
    assert!(descriptors_under(&fixture.root).is_empty());
    {
        let bin = Bin::open(&venv).unwrap();
        let descriptors = descriptors_under(&fixture.root);
        assert!(
            descriptors.len() >= 5,
            "fixture, outer, inner, venv and bin must stay pinned"
        );
        for descriptor in bin
            .ancestors
            .iter()
            .map(|a| a.directory.as_raw_fd())
            .chain([bin.dir.as_raw_fd()])
        {
            // SAFETY: every borrowed directory owns its live descriptor for this scope.
            let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        }
        for _ in 0..16 {
            bin.verify().unwrap();
        }
        assert_eq!(descriptors_under(&fixture.root), descriptors);
    }
    assert!(
        descriptors_under(&fixture.root).is_empty(),
        "Bin drop must release its retained chain"
    );
    let incomplete = fixture.root.join("incomplete/venv");
    fs::create_dir_all(&incomplete).unwrap();
    for _ in 0..16 {
        assert!(Bin::open(&incomplete).is_err());
    }
    assert!(
        descriptors_under(&fixture.root).is_empty(),
        "failed admission must release its retained chain"
    );
}
