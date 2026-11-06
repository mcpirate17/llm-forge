use super::*;
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::sync::{Arc, Barrier};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::resource_id("report-test").unwrap());
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn remove_retained_stages(root: &Path, error: &str, expected: usize) {
    let stages: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".report-pending")
        })
        .collect();
    assert_eq!(stages.len(), expected, "{error}");
    for entry in stages {
        let name = entry.file_name();
        assert!(error.contains(name.to_str().unwrap()), "{error}");
        let meta = fs::symlink_metadata(entry.path()).unwrap();
        assert!(meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.nlink() == 1);
        fs::remove_file(entry.path()).unwrap();
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
    private_file(&sentinel, b"keep");
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
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&real, &link).unwrap();
    let output = real.join("report");
    private_file(&output, b"original");
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
        let result = publish_locked(
            &output,
            b"complete",
            |parent| {
                assert!(crate::file_lock::Exclusive::try_new(
                    parent.open(".")?.into_std(),
                    "contending report publisher",
                )
                .is_err());
                ensure!(!fail, "injected pre-publication failure");
                Ok(())
            },
            || Ok(()),
        );
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
    private_file(&sentinel, b"original");
    let (parent, name) = open_parent(&output).unwrap();
    let (pending, held) = stage(&parent, b"complete").unwrap();
    symlink(&sentinel, &output).unwrap();
    assert!(check_destination(&parent, &name).is_err());
    cleanup(&parent, &pending, &held).unwrap();
    assert_eq!(fs::read(&sentinel).unwrap(), b"original");
    assert!(!root.0.join(pending).exists());
}

#[test]
fn guard_runs_once_after_staging_under_lock_and_failure_preserves_destination() {
    let root = Fixture::new();
    let output = root.0.join("report");
    private_file(&output, b"original");
    for fail in [true, false] {
        let mut calls = 0;
        let result = publish_checked(&output, b"replacement", || {
            calls += 1;
            assert_eq!(fs::read(&output)?, b"original");
            assert_eq!(fs::read_dir(&root.0)?.count(), 2);
            assert!(
                crate::file_lock::Exclusive::try_new(fs::File::open(&root.0)?, "contender")
                    .is_err()
            );
            ensure!(!fail, "held input drift");
            Ok(())
        });
        assert_eq!(calls, 1);
        assert_eq!(result.is_err(), fail);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
        assert_eq!(
            fs::read(&output).unwrap(),
            if fail {
                b"original".as_slice()
            } else {
                b"replacement"
            }
        );
    }
}

fn paired_fixture(root: &Fixture) -> (std::path::PathBuf, std::path::PathBuf) {
    let first = root.0.join("report");
    let second = root.0.join("ledger");
    private_file(&first, b"old report");
    private_file(&second, b"old ledger");
    (first, second)
}

#[test]
fn publication_refuses_shared_parent_and_shared_destination() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(publish(&first, b"new").is_err());
    assert!(publish_pair_checked((&first, b"new"), (&second, b"new"), || Ok(())).is_err());
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&second, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(publish(&second, b"new").is_err());
    assert!(publish_pair_checked((&first, b"new"), (&second, b"new"), || Ok(())).is_err());
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    assert_eq!(fs::read(&second).unwrap(), b"old ledger");
}

#[test]
fn publication_rechecks_permissions_after_guard() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let result = publish_checked(&first, b"new", || {
        fs::set_permissions(&first, fs::Permissions::from_mode(0o664))?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    fs::set_permissions(&first, fs::Permissions::from_mode(0o600)).unwrap();
    let result = publish_pair_checked((&first, b"new"), (&second, b"new"), || {
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o775))?;
        Ok(())
    });
    let error = format!("{:#}", result.unwrap_err());
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    assert_eq!(fs::read(&second).unwrap(), b"old ledger");
    remove_retained_stages(&root.0, &error, 2);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
}

#[test]
fn single_publish_retains_stage_when_parent_becomes_shared() {
    let root = Fixture::new();
    let output = root.0.join("report");
    private_file(&output, b"old");
    let error = publish_checked(&output, b"new", || {
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o775))?;
        Ok(())
    })
    .unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("group or world writable"));
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"old");
    remove_retained_stages(&root.0, &diagnostic, 1);
}

#[test]
fn paired_same_parent_stages_both_under_one_lock_before_guard() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let mut calls = 0;
    publish_pair_checked((&first, b"new report"), (&second, b"new ledger"), || {
        calls += 1;
        assert_eq!(fs::read(&first)?, b"old report");
        assert_eq!(fs::read(&second)?, b"old ledger");
        assert_eq!(fs::read_dir(&root.0)?.count(), 4);
        assert!(crate::file_lock::Exclusive::try_new(
            fs::File::open(&root.0)?,
            "contending pair publisher"
        )
        .is_err());
        Ok(())
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(fs::read(&first).unwrap(), b"new report");
    assert_eq!(fs::read(&second).unwrap(), b"new ledger");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
    for path in [&first, &second] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn paired_second_staging_failure_leaves_both_originals_and_no_pending_file() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let mut stages = 0;
    let error = publish_pair_using(
        (&first, b"new report"),
        (&second, b"new ledger"),
        || panic!("guard must not run before both stages succeed"),
        |parent, bytes| {
            stages += 1;
            ensure!(stages != 2, "injected second staging failure");
            stage(parent, bytes)
        },
        |_, _, _| panic!("no rename before both stages succeed"),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(stages, 2);
    assert!(error.contains("injected second staging failure"));
    assert!(error.contains(&format!("first path={first:?} renamed=false durable=false")));
    assert!(error.contains(&format!(
        "second path={second:?} renamed=false durable=false"
    )));
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    assert_eq!(fs::read(&second).unwrap(), b"old ledger");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
}

#[test]
fn paired_second_admission_failure_and_output_alias_never_replace_either_file() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let sentinel = root.0.join("sentinel");
    fs::rename(&second, &sentinel).unwrap();
    symlink(&sentinel, &second).unwrap();
    let error = publish_pair_using(
        (&first, b"new report"),
        (&second, b"new ledger"),
        || panic!("unsafe destination must refuse before guard"),
        |_, _| panic!("both destinations must be admitted before staging"),
        |_, _, _| panic!("unsafe destination must refuse before rename"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("renamed=false durable=false"));
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    assert_eq!(fs::read(&sentinel).unwrap(), b"old ledger");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 3);
    assert!(
        publish_pair_checked((&first, b"a"), (&root.0.join("./report"), b"b"), || {
            panic!("aliased destinations must refuse before guard")
        })
        .is_err()
    );
    assert_eq!(fs::read(&first).unwrap(), b"old report");
}

#[test]
fn paired_second_rename_failure_reports_first_durable_and_cleans_second_stage() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let mut renames = 0;
    let error = publish_pair_using(
        (&first, b"new report"),
        (&second, b"new ledger"),
        || Ok(()),
        stage,
        |parent, from, to| {
            renames += 1;
            ensure!(renames != 2, "injected second rename failure");
            parent.rename(from, parent, to)?;
            Ok(())
        },
    )
    .unwrap_err()
    .to_string();
    assert_eq!(renames, 2);
    assert!(error.contains("injected second rename failure"));
    assert!(error.contains(&format!("first path={first:?} renamed=true durable=true")));
    assert!(error.contains(&format!(
        "second path={second:?} renamed=false durable=false"
    )));
    assert_eq!(fs::read(&first).unwrap(), b"new report");
    assert_eq!(fs::read(&second).unwrap(), b"old ledger");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
}

#[test]
fn paired_stale_input_guard_refuses_after_staging_without_replacing_outputs() {
    let root = Fixture::new();
    let (first, second) = paired_fixture(&root);
    let input = root.0.join("input");
    private_file(&input, b"captured input");
    let (held, _) = crate::candidate_inputs::repository_live(&input, 1024).unwrap();
    assert!(
        publish_pair_checked((&first, b"new report"), (&second, b"new ledger"), || {
            assert_eq!(fs::read_dir(&root.0)?.count(), 5);
            fs::write(&input, b"changed input")?;
            held.verify()
        })
        .is_err()
    );
    assert_eq!(fs::read(&first).unwrap(), b"old report");
    assert_eq!(fs::read(&second).unwrap(), b"old ledger");
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 3);
}

#[test]
fn paired_cross_parent_locks_serialize_opposite_publication_orders() {
    let root = Fixture::new();
    let left = root.0.join("left");
    let right = root.0.join("right");
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    fs::set_permissions(&left, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&right, fs::Permissions::from_mode(0o700)).unwrap();
    let first = left.join("report");
    let second = right.join("ledger");
    let barrier = Arc::new(Barrier::new(2));
    let mut jobs = Vec::new();
    for value in [1_u8, 2] {
        let (first, second, left, right, barrier) = (
            first.clone(),
            second.clone(),
            left.clone(),
            right.clone(),
            barrier.clone(),
        );
        jobs.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..16 {
                let bytes = vec![value; 4096];
                let pair = if value == 1 {
                    (&first, &second)
                } else {
                    (&second, &first)
                };
                publish_pair_checked((pair.0, &bytes), (pair.1, &bytes), || {
                    for parent in [&left, &right] {
                        assert!(crate::file_lock::Exclusive::try_new(
                            fs::File::open(parent)?,
                            "contending cross-parent publisher"
                        )
                        .is_err());
                    }
                    Ok(())
                })
                .unwrap();
            }
        }));
    }
    for job in jobs {
        job.join().unwrap();
    }
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    assert_eq!(fs::read_dir(left).unwrap().count(), 1);
    assert_eq!(fs::read_dir(right).unwrap().count(), 1);
}
