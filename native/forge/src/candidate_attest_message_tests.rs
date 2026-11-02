use super::*;
use std::os::unix::fs::symlink;
fn completed() -> Completed {
    Completed {
        bytes: vec![],
        tree: "a".repeat(40),
        policy: "b".repeat(64),
        digest: "c".repeat(64),
    }
}
#[test]
fn exact_trailers_are_idempotent_and_partial_duplicate_or_conflicting_refuse() {
    let receipt = completed();
    let output = append(b"subject\n", &receipt).unwrap();
    let text = std::str::from_utf8(&output).unwrap();
    assert!(text.starts_with("subject\n\nGovernance-Tree: "));
    assert!(trailers(&output).unwrap().iter().all(Option::is_some));
    assert!(trailers(b"subject\nGovernance-Tree: abc\n")
        .unwrap()
        .iter()
        .any(Option::is_none));
    assert!(previous_receipt(
        Path::new("/missing"),
        [Some(receipt.tree.as_str()), None, None],
        &receipt
    )
    .is_err());
    assert!(previous_receipt(
        Path::new("/missing"),
        [
            Some("conflict"),
            Some(receipt.policy.as_str()),
            Some(receipt.digest.as_str())
        ],
        &receipt
    )
    .is_err());
    let duplicate = format!("{text}Governance-Tree: {}\n", receipt.tree);
    assert!(trailers(duplicate.as_bytes()).is_err());
    let altered = text.replace(&receipt.digest, &"d".repeat(64));
    assert_ne!(
        trailers(altered.as_bytes()).unwrap()[2],
        Some(receipt.digest.as_str())
    );
}
#[test]
fn message_rejects_symlinks_hardlinks_and_observed_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("message");
    fs::write(&path, b"subject\n").unwrap();
    let link = temp.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(Message::open(&link).is_err());
    let hard = temp.path().join("hard");
    fs::hard_link(&path, &hard).unwrap();
    assert!(Message::open(&path).is_err());
    fs::remove_file(hard).unwrap();
    let held = Message::open(&path).unwrap();
    fs::write(&path, b"changed\n").unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn message_rejects_fifo_and_oversize() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let temp = tempfile::tempdir().unwrap();
    let fifo = temp.path().join("fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(Message::open(&fifo).is_err());
    let large = temp.path().join("large");
    fs::write(&large, vec![b'x'; MAX_MESSAGE + 1]).unwrap();
    assert!(Message::open(&large).is_err());
}
