use super::*;
use std::fs;
#[test]
fn native_crlf_clean_smudge_and_bound_attributes() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join(".gitattributes"), b"*.txt text eol=crlf\n").unwrap();
    let source = Source::open(dir.path()).unwrap();
    let lease = SyncLease::acquire(dir.path(), &mut || Ok(())).unwrap();
    let policy = Policy::capture(&source, &lease, "a.txt").unwrap();
    assert_eq!(
        policy
            .convert(&source, &lease, "a.txt", b"a\r\nb\r\n", true)
            .unwrap(),
        b"a\nb\n"
    );
    assert_eq!(
        policy
            .convert(&source, &lease, "a.txt", b"a\nb\n", false)
            .unwrap(),
        b"a\r\nb\r\n"
    );
    fs::write(dir.path().join(".gitattributes"), b"*.txt -text\n").unwrap();
    assert!(policy
        .convert(&source, &lease, "a.txt", b"a\r\n", true)
        .is_err());
}
#[test]
fn external_filter_is_refused_before_conversion() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    fs::write(
        dir.path().join(".gitattributes"),
        b"*.txt filter=external\n",
    )
    .unwrap();
    let source = Source::open(dir.path()).unwrap();
    let lease = SyncLease::acquire(dir.path(), &mut || Ok(())).unwrap();
    assert!(Policy::capture(&source, &lease, "a.txt").is_err());
}
#[test]
fn binary_attribute_preserves_binary_and_crlf_bytes() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join(".gitattributes"), b"*.bin -text\n").unwrap();
    let source = Source::open(dir.path()).unwrap();
    let lease = SyncLease::acquire(dir.path(), &mut || Ok(())).unwrap();
    let policy = Policy::capture(&source, &lease, "a.bin").unwrap();
    let bytes = b"\0one\r\ntwo\n";
    assert_eq!(
        policy
            .convert(&source, &lease, "a.bin", bytes, true)
            .unwrap(),
        bytes
    );
    assert_eq!(
        policy
            .convert(&source, &lease, "a.bin", bytes, false)
            .unwrap(),
        bytes
    );
}
