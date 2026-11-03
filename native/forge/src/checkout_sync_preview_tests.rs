use super::*;
use crate::checkout_sync_worktree::Content;
use git2::{Repository, Signature};
use std::{fs, path::Path};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    fs::write(dir.path().join("a"), b"one\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(dir.path().join("b"), b"old\n").unwrap();
    let mut index = repo.index().unwrap();
    for path in ["a", "b"] {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let old = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let mut builder = repo.treebuilder(Some(&tree)).unwrap();
    builder
        .insert("b", repo.blob(b"new\n").unwrap(), 0o100644)
        .unwrap();
    let next = repo.find_tree(builder.write().unwrap()).unwrap();
    let parent = repo.find_commit(old).unwrap();
    let new = repo
        .commit(None, &sig, &sig, "incoming", &next, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", new, false, "fixture")
        .unwrap();
    dir
}
#[test]
fn preview_preserves_raw_crlf_and_shares_smudge_planning() {
    let dir = fixture();
    fs::write(dir.path().join(".gitattributes"), b"* text eol=crlf\n").unwrap();
    let raw = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n";
    fs::write(dir.path().join("a"), raw).unwrap();
    fs::write(dir.path().join("b"), b"old\r\n").unwrap();
    let preflight = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let index = fs::read(&preflight.index_path).unwrap();
    let plan = inspect(&preflight, &mut || Ok(())).unwrap();
    assert!(!plan.operations.iter().any(|op| op.path == "a"));
    let op = plan.operations.iter().find(|op| op.path == "b").unwrap();
    assert!(matches!(&op.after, Some(Content::File { bytes, .. }) if bytes == b"new\r\n"));
    assert_eq!(fs::read(dir.path().join("a")).unwrap(), raw);
    assert_eq!(fs::read(&preflight.index_path).unwrap(), index);
}
#[test]
fn preview_refuses_hardlinks_and_unsupported_filters() {
    for hardlink in [true, false] {
        let dir = fixture();
        if hardlink {
            fs::hard_link(dir.path().join("a"), dir.path().join("peer-link")).unwrap();
        } else {
            fs::write(dir.path().join(".gitattributes"), b"a filter=custom\n").unwrap();
        }
        let preflight = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
        let error = inspect(&preflight, &mut || Ok(())).err().unwrap();
        assert!(error
            .downcast_ref::<checkout_sync_plan::Blocked>()
            .is_none());
        let message = format!("{error:#}");
        assert!(
            message.contains(if hardlink {
                "hard-linked"
            } else {
                "filter unsupported"
            }),
            "{message}"
        );
    }
}
#[test]
fn preview_reports_empty_directory_collision_as_blocked() {
    let dir = fixture();
    fs::remove_file(dir.path().join("b")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    let preflight = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let error = inspect(&preflight, &mut || Ok(())).err().unwrap();
    assert!(error
        .downcast_ref::<checkout_sync_plan::Blocked>()
        .is_some());
    assert!(dir.path().join("b").is_dir());
    assert_eq!(fs::read_dir(dir.path().join("b")).unwrap().count(), 0);
}
#[test]
fn preview_validates_executor_index_extensions_without_writing_them() {
    use sha1::{Digest, Sha1};
    let dir = fixture();
    let path = dir.path().join(".git/index");
    let mut raw = fs::read(&path).unwrap();
    raw.truncate(raw.len() - 20);
    raw.extend_from_slice(b"TEST\0\0\0\0");
    let digest = Sha1::digest(&raw);
    raw.extend_from_slice(&digest);
    fs::write(&path, &raw).unwrap();
    let preflight = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let error = inspect(&preflight, &mut || Ok(())).err().unwrap();
    assert!(format!("{error:#}").contains("unsupported Git index extension"));
    assert_eq!(fs::read(&path).unwrap(), raw);
}
#[test]
fn preview_honors_cancellation_and_never_materializes_a_snapshot() {
    let dir = fixture();
    let preflight = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let error = inspect(&preflight, &mut || anyhow::bail!("cancelled by test"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("cancelled by test"));
    assert!(!dir.path().join(".git/refs/snapshots").exists());
    preflight.verify().unwrap();
}
