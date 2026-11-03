use super::*;
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::{Arc, Barrier};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::resource_id("report-test").unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn publishes_and_replaces_private_complete_bytes() {
    let root = Fixture::new();
    let output = root.0.join("report.json");
    publish(&output, b"first").unwrap();
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    publish(&output, b"second").unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"second");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
}

#[test]
fn unsafe_targets_never_touch_sentinel() {
    let root = Fixture::new();
    let sentinel = root.0.join("sentinel");
    let output = root.0.join("output");
    fs::write(&sentinel, b"keep").unwrap();
    symlink(&sentinel, &output).unwrap();
    assert!(publish(&output, b"changed").is_err());
    fs::remove_file(&output).unwrap();
    fs::hard_link(&sentinel, &output).unwrap();
    assert!(publish(&output, b"changed").is_err());
    fs::remove_file(&output).unwrap();
    let fifo = CString::new(output.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(publish(&output, b"changed").is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
}

#[test]
fn symlink_parent_traversal_and_oversize_fail_without_clobber() {
    let root = Fixture::new();
    let real = root.0.join("real");
    let link = root.0.join("link");
    fs::create_dir(&real).unwrap();
    symlink(&real, &link).unwrap();
    let output = real.join("report");
    fs::write(&output, b"original").unwrap();
    assert!(publish(&link.join("report"), b"changed").is_err());
    assert!(publish(&real.join("../report"), b"changed").is_err());
    assert!(publish(&output, &vec![0; MAX_BYTES + 1]).is_err());
    assert!(publish(&root.0.join("absent/report"), b"changed").is_err());
    assert_eq!(fs::read(&output).unwrap(), b"original");
    assert_eq!(fs::read_dir(&real).unwrap().count(), 1);
}

#[test]
fn concurrent_cooperating_publishers_leave_one_complete_report() {
    let root = Fixture::new();
    let output = root.0.join("report");
    let barrier = Arc::new(Barrier::new(8));
    let mut jobs = Vec::new();
    for value in 0..8_u8 {
        let path = output.clone();
        let barrier = barrier.clone();
        jobs.push(std::thread::spawn(move || {
            let bytes = vec![value; 64 * 1024];
            barrier.wait();
            for _ in 0..16 {
                publish(&path, &bytes).unwrap();
            }
        }));
    }
    for job in jobs {
        job.join().unwrap();
    }
    let result = fs::read(&output).unwrap();
    assert_eq!(result.len(), 64 * 1024);
    assert!(result.iter().all(|byte| *byte == result[0]));
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
}

#[test]
fn parent_lease_serializes_publishers_and_releases_after_success_or_failure() {
    let root = Fixture::new();
    let output = root.0.join("report");
    for fail in [false, true] {
        let result = publish_locked(&output, b"complete", |parent| {
            assert!(crate::file_lock::Exclusive::try_new(
                parent.open(".")?.into_std(),
                "contending report publisher",
            )
            .is_err());
            ensure!(!fail, "injected pre-publication failure");
            Ok(())
        });
        assert_eq!(result.is_err(), fail);
        let lease = crate::file_lock::Exclusive::try_new(
            fs::File::open(&root.0).unwrap(),
            "subsequent report publisher",
        )
        .unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"complete");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
        drop(lease);
    }
}

#[test]
fn pre_rename_rejection_removes_only_owned_staging_file() {
    let root = Fixture::new();
    let output = root.0.join("report");
    let sentinel = root.0.join("sentinel");
    fs::write(&sentinel, b"original").unwrap();
    let (parent, name) = open_parent(&output).unwrap();
    let (pending, held) = stage(&parent, b"complete").unwrap();
    symlink(&sentinel, &output).unwrap();
    assert!(check_destination(&parent, &name).is_err());
    cleanup(&parent, &pending, &held).unwrap();
    assert_eq!(fs::read(&sentinel).unwrap(), b"original");
    assert!(!root.0.join(pending).exists());
}
