//! Observable durability boundaries, failure cleanup and path-replacement safety.
use super::*;
use crate::candidate_snapshot::directory_sync::SYNC_HOOK;
use std::{
    cell::RefCell,
    fs, io,
    os::unix::fs::{symlink, PermissionsExt},
    rc::Rc,
};

struct HookGuard;
impl HookGuard {
    fn install(callback: impl FnMut(&Path) -> io::Result<()> + 'static) -> Self {
        SYNC_HOOK.with(|hook| {
            let mut hook = hook.borrow_mut();
            assert!(
                hook.is_none(),
                "directory-sync hook already active on this test thread"
            );
            *hook = Some(Box::new(callback));
        });
        Self
    }
}
impl Drop for HookGuard {
    fn drop(&mut self) {
        SYNC_HOOK.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}
fn destination(fixture: &Fixture) -> PathBuf {
    let parent = fixture.root.join("destination");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    parent
}
fn staging(parent: &Path) -> PathBuf {
    let entries: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected only the unpublished owned staging tree"
    );
    assert!(entries[0].is_dir());
    entries[0].clone()
}
fn assert_empty(parent: &Path) {
    assert_eq!(
        fs::read_dir(parent).unwrap().count(),
        0,
        "failed snapshot leaked staging data"
    );
}
fn nested_tree(fixture: &Fixture) -> String {
    fixture.tree(&[
        ("root.txt", b"root", 0o100644),
        ("branch/first", b"first", 0o100644),
        ("branch/second", b"second", 0o100644),
        ("branch/deep/run", b"#!/bin/sh\nexit 0\n", 0o100755),
        ("links/only", b"../branch/first", 0o120000),
    ])
}
fn assert_populated(root: &Path) {
    for (name, content, mode) in [
        ("root.txt", b"root".as_slice(), 0o644),
        ("branch/first", b"first".as_slice(), 0o644),
        ("branch/second", b"second".as_slice(), 0o644),
        ("branch/deep/run", b"#!/bin/sh\nexit 0\n".as_slice(), 0o755),
    ] {
        assert_eq!(fs::read(root.join(name)).unwrap(), content);
        assert_eq!(
            fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
    for name in ["branch", "branch/deep", "links"] {
        let metadata = fs::metadata(root.join(name)).unwrap();
        assert!(metadata.is_dir());
        assert_eq!(metadata.permissions().mode() & 0o700, 0o700);
        assert_eq!(metadata.permissions().mode() & 0o022, 0);
    }
    assert!(fs::symlink_metadata(root.join("links/only"))
        .unwrap()
        .is_symlink());
    assert_eq!(
        fs::read_link(root.join("links/only")).unwrap(),
        Path::new("../branch/first")
    );
    assert_eq!(fs::read(root.join("links/only")).unwrap(), b"first");
    assert_eq!(
        fs::metadata(root).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn complete_files_modes_and_source_links_precede_one_flush_per_directory() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let tree = nested_tree(&fixture);
    let observed = Rc::new(RefCell::new(Vec::new()));
    let events = observed.clone();
    let watched = parent.clone();
    let hook = HookGuard::install(move |path| {
        assert_populated(&staging(&watched));
        events.borrow_mut().push(path.to_owned());
        Ok(())
    });
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &parent.join("published"),
        Limits::default(),
    )
    .unwrap();
    drop(hook);
    let events = observed.borrow();
    let expected = ["", "branch", "branch/deep", "links"];
    assert_eq!(events.len(), expected.len());
    for path in expected {
        assert_eq!(
            events
                .iter()
                .filter(|seen| seen.as_path() == Path::new(path))
                .count(),
            1,
            "{events:?}"
        );
    }
    for (index, child) in events.iter().enumerate() {
        for (parent_index, ancestor) in events.iter().enumerate() {
            if child != ancestor && child.starts_with(ancestor) {
                assert!(
                    index < parent_index,
                    "directory flushed before its descendant: {events:?}"
                );
            }
        }
    }
    assert!(!parent.join("published").exists());
    let staged = snapshot.path();
    snapshot.discard().unwrap();
    assert!(!staged.exists());
    assert_empty(&parent);
}

#[test]
fn child_parent_and_root_flush_errors_refuse_success_and_dispose_owned_staging() {
    for target in ["branch/deep", "branch", ""] {
        let fixture = Fixture::new();
        let parent = destination(&fixture);
        let tree = nested_tree(&fixture);
        let target = PathBuf::from(target);
        let injected = Rc::new(RefCell::new(false));
        let happened = injected.clone();
        let hook = HookGuard::install(move |path| {
            if path == target.as_path() {
                *happened.borrow_mut() = true;
                Err(io::Error::other("injected directory flush failure"))
            } else {
                Ok(())
            }
        });
        let error = Snapshot::create(
            &fixture.source(),
            &tree,
            &parent.join("published"),
            Limits::default(),
        )
        .err()
        .expect("snapshot accepted a failed flush");
        drop(hook);
        assert!(*injected.borrow());
        assert!(
            format!("{error:#}").contains("injected directory flush failure"),
            "{error:#}"
        );
        assert_empty(&parent);
        assert_eq!(fs::read_dir(fixture.root.join("repo")).unwrap().count(), 1);
    }
}

#[test]
fn directory_symlink_replacement_before_flush_refuses_and_preserves_outside_tree() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let outside = fixture.root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(outside.join("sentinel"), b"outside must survive").unwrap();
    fs::set_permissions(outside.join("sentinel"), fs::Permissions::from_mode(0o600)).unwrap();
    let watched = parent.clone();
    let target = outside.clone();
    let replaced = Rc::new(RefCell::new(false));
    let happened = replaced.clone();
    let hook = HookGuard::install(move |path| {
        if path == Path::new("branch/deep") {
            let root = staging(&watched);
            fs::rename(root.join("branch/deep"), root.join("branch/displaced"))?;
            symlink(&target, root.join("branch/deep"))?;
            *happened.borrow_mut() = true;
        }
        Ok(())
    });
    let error = Snapshot::create(
        &fixture.source(),
        &nested_tree(&fixture),
        &parent.join("published"),
        Limits::default(),
    )
    .err()
    .expect("snapshot followed a replaced directory");
    drop(hook);
    assert!(*replaced.borrow());
    assert!(!format!("{error:#}").is_empty());
    assert_empty(&parent);
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"outside must survive"
    );
    assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
}

#[test]
fn equivalent_replacement_directory_is_not_the_recorded_directory() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let watched = parent.clone();
    let hook = HookGuard::install(move |path| {
        if path == Path::new("branch/deep") {
            let root = staging(&watched);
            fs::rename(root.join("branch/deep"), root.join("branch/displaced"))?;
            fs::create_dir(root.join("branch/deep"))?;
            fs::set_permissions(root.join("branch/deep"), fs::Permissions::from_mode(0o755))?;
            fs::write(root.join("branch/deep/run"), b"#!/bin/sh\nexit 0\n")?;
            fs::set_permissions(
                root.join("branch/deep/run"),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        Ok(())
    });
    let error = Snapshot::create(
        &fixture.source(),
        &nested_tree(&fixture),
        &parent.join("published"),
        Limits::default(),
    )
    .err()
    .expect("snapshot accepted replacement inode");
    drop(hook);
    assert!(
        format!("{error:#}").contains("directory identity changed"),
        "{error:#}"
    );
    assert_empty(&parent);
}

#[test]
fn hook_guard_resets_on_panic_on_the_same_thread() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let tree = nested_tree(&fixture);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = HookGuard::install(|_| panic!("authored flush hook unwind"));
        let _ = Snapshot::create(
            &fixture.source(),
            &tree,
            &parent.join("published"),
            Limits::default(),
        );
    }));
    assert!(result.is_err());
    SYNC_HOOK.with(|hook| assert!(hook.borrow().is_none()));
    assert_empty(&parent);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &parent.join("published"),
        Limits::default(),
    )
    .unwrap();
    snapshot.discard().unwrap();
    assert_empty(&parent);
}

#[test]
fn many_sibling_directories_complete_under_a_private_child_descriptor_limit() {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "candidate_snapshot::tests::directory_sync_tests::bounded_descriptor_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FORGE_TEST_DIRECTORY_SYNC_FD_CHILD", "1");
    let output = crate::process_probe::run(
        &mut command,
        std::time::Duration::from_secs(30),
        1024 * 1024,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

#[test]
#[ignore = "private subprocess entrypoint; run through the descriptor-limit parent test"]
fn bounded_descriptor_child() {
    assert_eq!(
        std::env::var("FORGE_TEST_DIRECTORY_SYNC_FD_CHILD").unwrap(),
        "1"
    );
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let names: Vec<_> = (0..192)
        .map(|number| format!("sibling-{number:03}/file"))
        .collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), b"payload".as_slice(), 0o100644))
        .collect();
    let tree = fixture.tree(&files);
    let mut previous = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // Only this dedicated one-test subprocess changes its own descriptor limit.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut previous) },
        0
    );
    assert!(previous.rlim_cur >= 64);
    let bounded = libc::rlimit {
        rlim_cur: 64,
        rlim_max: previous.rlim_max,
    };
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &bounded) }, 0);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &parent.join("published"),
        Limits::default(),
    )
    .unwrap();
    for name in &names {
        assert_eq!(fs::read(snapshot.path().join(name)).unwrap(), b"payload");
    }
    snapshot.discard().unwrap();
    assert_empty(&parent);
}

#[test]
fn replaced_ancestor_is_refused_before_reopening_its_unchanged_child() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let watched = parent.clone();
    let injected = Rc::new(RefCell::new(false));
    let happened = injected.clone();
    let later = Rc::new(RefCell::new(Vec::new()));
    let after_replacement = later.clone();
    let hook = HookGuard::install(move |path| {
        if *happened.borrow() {
            after_replacement.borrow_mut().push(path.to_owned());
        } else if path == Path::new("branch/deep") {
            let root = staging(&watched);
            let original = root.join("branch");
            let moved = root.join("displaced");
            let permissions = fs::metadata(&original)?.permissions();
            fs::rename(&original, &moved)?;
            fs::create_dir(&original)?;
            fs::set_permissions(&original, permissions)?;
            // Preserve every child inode; only the intermediate directory changes.
            for child in fs::read_dir(&moved)? {
                let child = child?;
                fs::rename(child.path(), original.join(child.file_name()))?;
            }
            *happened.borrow_mut() = true;
        }
        Ok(())
    });
    let error = Snapshot::create(
        &fixture.source(),
        &nested_tree(&fixture),
        &parent.join("published"),
        Limits::default(),
    )
    .err()
    .expect("snapshot accepted changed ancestor");
    drop(hook);
    assert!(*injected.borrow());
    assert!(
        format!("{error:#}").contains("directory identity changed: branch"),
        "{error:#}"
    );
    assert!(
        later.borrow().is_empty(),
        "continued directory flush after ancestor replacement"
    );
    assert_empty(&parent);
}

#[test]
fn held_root_permission_change_before_flush_refuses_and_cleans_staging() {
    let fixture = Fixture::new();
    let parent = destination(&fixture);
    let watched = parent.clone();
    let hook = HookGuard::install(move |path| {
        if path.as_os_str().is_empty() {
            fs::set_permissions(staging(&watched), fs::Permissions::from_mode(0o750))?;
        }
        Ok(())
    });
    let error = Snapshot::create(
        &fixture.source(),
        &nested_tree(&fixture),
        &parent.join("published"),
        Limits::default(),
    )
    .err()
    .expect("snapshot accepted changed root permissions");
    drop(hook);
    assert!(
        format!("{error:#}").contains("directory identity changed"),
        "{error:#}"
    );
    assert_empty(&parent);
}
