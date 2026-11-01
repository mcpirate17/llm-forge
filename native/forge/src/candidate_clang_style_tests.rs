use super::*;
use std::os::unix::fs::symlink;

fn entry(path: &str, bytes: &[u8]) -> TreeEntry {
    TreeEntry {
        path: path.into(),
        mode: "100644".into(),
        object_type: "blob".into(),
        oid: Oid::hash_object(ObjectType::Blob, bytes)
            .unwrap()
            .to_string(),
        size: Some(bytes.len() as u64),
    }
}

fn fixture() -> (tempfile::TempDir, Dir) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Dir::open_ambient_dir(tmp.path(), cap_std::ambient_authority()).unwrap();
    (tmp, dir)
}

#[test]
fn nearest_style_precedence_and_fallback_are_bound() {
    let (tmp, root) = fixture();
    fs::create_dir(tmp.path().join("src")).unwrap();
    let parent = b"BasedOnStyle: LLVM\nColumnLimit: 90\n";
    let child = b"BasedOnStyle: Google\n";
    fs::write(tmp.path().join(".clang-format"), parent).unwrap();
    fs::write(tmp.path().join("src/_clang-format"), child).unwrap();
    let entries = [
        entry(".clang-format", parent),
        entry("src/_clang-format", child),
    ];
    let files = vec!["src/main.cc".into(), "main.cc".into()];
    let plan = Plan::from_entries(root, tmp.path().to_path_buf(), &entries, &files).unwrap();
    assert!(plan
        .argument_for("src/main.cc")
        .unwrap()
        .ends_with("/src/_clang-format"));
    assert!(plan
        .argument_for("main.cc")
        .unwrap()
        .ends_with("/.clang-format"));
    plan.verify().unwrap();
    fs::write(
        tmp.path().join("src/_clang-format"),
        b"BasedOnStyle: LLVM\n",
    )
    .unwrap();
    assert!(plan.verify().is_err());
    let (tmp, root) = fixture();
    let plan = Plan::from_entries(root, tmp.path().to_path_buf(), &[], &["x.cc".into()]).unwrap();
    assert_eq!(plan.argument_for("x.cc").unwrap(), "--style=LLVM");
    fs::write(tmp.path().join(".clang-format"), b"BasedOnStyle: LLVM\n").unwrap();
    assert!(plan.verify().is_err());
}

#[test]
fn decoded_inheritance_aliases_tags_and_depth_refuse() {
    assert!(admissible(b"BasedOnStyle: \"InheritParent\\x43onfig\"\n").is_err());
    assert!(admissible(b"a: &shared [LLVM]\nb: *shared\n").is_err());
    assert!(admissible(b"a: !tag LLVM\n").is_err());
    assert!(admissible(format!("a: {}x{}", "[".repeat(65), "]".repeat(65)).as_bytes()).is_err());
    assert!(admissible(
        b"BasedOnStyle: LLVM\nIncludeCategories:\n  - Regex: '^<.*>'\n    Priority: 1\n"
    )
    .is_ok());
}

#[test]
fn untracked_ignore_and_symlinked_style_refuse() {
    let (tmp, root) = fixture();
    fs::write(tmp.path().join(".clang-format-ignore"), b"*.cc\n").unwrap();
    assert!(Plan::from_entries(root, tmp.path().to_path_buf(), &[], &["x.cc".into()]).is_err());
    fs::remove_file(tmp.path().join(".clang-format-ignore")).unwrap();
    symlink("/etc/passwd", tmp.path().join(".clang-format")).unwrap();
    let root = Dir::open_ambient_dir(tmp.path(), cap_std::ambient_authority()).unwrap();
    assert!(Plan::from_entries(root, tmp.path().to_path_buf(), &[], &["x.cc".into()]).is_err());
}
