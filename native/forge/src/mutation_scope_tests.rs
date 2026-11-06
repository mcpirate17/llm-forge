use super::*;
use std::fs;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(status.status.success(), "{args:?}: {status:?}");
}

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    fs::write(root.join("kept.py"), "a = 1\n").expect("write");
    fs::write(root.join("changed.py"), "b = 1\n").expect("write");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["branch", "base"]);
    dir
}

#[test]
fn committed_branch_changes_are_the_scope() {
    let dir = repository();
    let root = dir.path();
    fs::write(root.join("changed.py"), "b = 2\n").expect("write");
    git(root, &["commit", "-qam", "change"]);

    let scope = branch_scope("base", root, "lane").expect("scope");

    assert_eq!(scope, ["changed.py"]);
}

#[test]
fn an_unchanged_branch_is_refused_rather_than_widened() {
    let dir = repository();

    let error = branch_scope("base", dir.path(), "lane").expect_err("empty");

    assert!(error.starts_with("nothing changed against base"), "{error}");
}

#[test]
fn dirt_without_an_active_claim_is_refused() {
    let dir = repository();
    fs::write(dir.path().join("kept.py"), "a = 2\n").expect("write");

    let error = changed_sources("base", dir.path(), "lane").expect_err("no claim");

    assert!(
        error.contains("'lane' has no active ownership claim"),
        "{error}"
    );
}

#[test]
fn a_bad_base_is_a_refusal_naming_the_git_command() {
    let dir = repository();

    let error = changed_sources("missing", dir.path(), "lane").expect_err("bad base");

    assert!(
        error.contains("git diff --name-only missing...HEAD"),
        "{error}"
    );
}

#[test]
fn explicit_scope_refuses_escapes_and_missing_files() {
    let dir = repository();
    let root = dir.path();

    for raw in ["../kept.py", "/kept.py", "./kept.py"] {
        let error = explicit_scope(&[raw.to_string()], root).expect_err(raw);
        assert!(error.contains("repository-relative"), "{error}");
    }
    let error = explicit_scope(&["absent.py".to_string()], root).expect_err("missing");
    assert_eq!(error, "--only source does not exist: absent.py");
    assert!(explicit_scope(&[], root).is_err());
}

#[test]
fn explicit_scope_sorts_and_deduplicates() {
    let dir = repository();
    let paths = ["kept.py", "changed.py", "kept.py"].map(String::from);

    let scope = explicit_scope(&paths, dir.path()).expect("scope");

    assert_eq!(scope, ["changed.py", "kept.py"]);
}

#[test]
fn extra_tests_require_a_real_test_target() {
    let dir = repository();
    let root = dir.path();
    fs::write(root.join("test_kept.py"), "def test_a():\n    pass\n").expect("write");

    let parsed = extra_tests(&["kept.py=test_kept.py".into()], root).expect("pairs");
    assert_eq!(parsed["kept.py"], ["test_kept.py"]);
    fs::create_dir(root.join("tests")).expect("tests");
    fs::write(root.join("tests/driver.cpp"), "int main() { return 0; }").expect("driver");
    let parsed =
        extra_tests(&["kept.py=tests/driver.cpp".into()], root).expect("root test directory");
    assert_eq!(parsed["kept.py"], ["tests/driver.cpp"]);

    let error = extra_tests(&["kept.py=changed.py".into()], root).expect_err("not a test");
    assert_eq!(error, "--extra-test target is not a test file: changed.py");
    let error = extra_tests(&["kept.py".into()], root).expect_err("no separator");
    assert!(error.contains("wants SOURCE=TEST"), "{error}");
}
