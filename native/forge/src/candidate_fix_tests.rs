use super::*;
use tempfile::TempDir;
fn repo() -> (TempDir, SourceRoot) {
    let temp = TempDir::new().unwrap();
    let root = SourceRoot::open(temp.path()).unwrap();
    (temp, root)
}
#[test]
fn encoding_admission_preserves_exact_utf8_and_refuses_reinterpretation() {
    for bytes in [
        b"x = 1\r\n".as_slice(),
        b"",
        "# coding: utf-8\nname = 'café'\n".as_bytes(),
    ] {
        validate_encoding(bytes).unwrap();
    }
    for bytes in [
        b"# coding: latin-1\nx=1\n".as_slice(),
        b"# first\n# encoding=cp1252\n",
        b"\xef\xbb\xbfx=1\n",
        b"x='\xff'\n",
        b"x=\0\n",
    ] {
        assert!(validate_encoding(bytes).is_err());
    }
}
#[test]
fn concrete_source_scope_rejects_aliases_duplicates_and_nonpython() {
    use std::os::unix::fs::symlink;
    let (temp, root) = repo();
    fs::write(temp.path().join("a.py"), "x=1\r\n").unwrap();
    fs::write(temp.path().join("b.txt"), "x=1\n").unwrap();
    symlink(temp.path().join("a.py"), temp.path().join("alias.py")).unwrap();
    for paths in [
        vec![PathBuf::from("a.py"), PathBuf::from("a.py")],
        vec![PathBuf::from("alias.py")],
        vec![PathBuf::from("../a.py")],
        vec![PathBuf::from("b.txt")],
        vec![PathBuf::from(".")],
    ] {
        assert!(admit_files(&root, temp.path(), &paths).is_err());
    }
    let files = admit_files(&root, temp.path(), &["a.py".into()]).unwrap();
    assert_eq!(files[0].snapshot.content().as_bytes(), b"x=1\r\n");
    fs::write(temp.path().join("a.py"), "x=2\r\n").unwrap();
    assert!(files[0].binding.verify().is_err());
}
#[test]
fn configuration_refuses_source_kind_remapping_and_interpolation() {
    config::test_settings("extension = {py = 'ipynb'}").unwrap_err();
    config::test_settings("src = ['$HOME/src']").unwrap_err();
    config::test_settings("extension = {py = 'python', pyi = 'pyi'}\nline-length = 88").unwrap();
}
