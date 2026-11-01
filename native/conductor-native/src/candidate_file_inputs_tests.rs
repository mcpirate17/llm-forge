use super::*;
use serde_json::json;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn policy() -> Value {
    json!({"kind":"command", "file_input":"cppcheck-list", "command":["cppcheck", "--file-list={changed_files_file}"], "classes":[], "exclude_classes":[], "run_on_deletions":false})
}
fn change(name: &str, mode: &str) -> Value {
    json!({"path":name,"new_mode":mode,"classes":[]})
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::native_id::new("file-input-test").unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn root(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, cap_std::ambient_authority()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn parse_protocol_before_empty_selection() {
    let mut p = policy();
    p["file_input"] = json!(null);
    assert!(crate::candidate_checks::select_files(&[], &p).is_err());
    p["file_input"] = json!("unknown");
    assert!(crate::candidate_checks::select_files(&[], &p).is_err());
    p = policy();
    p["kind"] = json!("builtin");
    assert!(parse(&p).is_err());
    p = policy();
    p["command"] = json!(["cppcheck", "{files}", "--file-list={changed_files_file}"]);
    assert!(parse(&p).is_err());
    p["command"] = json!([
        "cppcheck",
        "--file-list={changed_files_file}",
        "--file-list={changed_files_file}"
    ]);
    assert!(parse(&p).is_err());
    p["command"] = json!(["cppcheck", "--file-list={changed_files_file}", null]);
    assert!(parse(&p).is_err());
    p["command"] = json!([
        "cppcheck",
        "--file-list={changed_files_file}",
        "--file-list=other"
    ]);
    assert!(parse(&p).is_err());
    let mut default = policy();
    default.as_object_mut().unwrap().remove("file_input");
    assert!(crate::candidate_checks::select_files(&[], &default).is_ok());
}

#[test]
fn selected_paths_are_canonical_and_regular_git_modes() {
    let p = policy();
    for bad in [
        "a\\b.cpp",
        "/a.cpp",
        "x/../a.cpp",
        "a//b.cpp",
        "a\nb.cpp",
        "a\rb.cpp",
        "a\0b.cpp",
        &format!("{}.cpp", "x".repeat(4096)),
    ] {
        assert!(
            crate::candidate_checks::select_files(&[change(bad, "100644")], &p).is_err(),
            "{bad:?}"
        );
    }
    for mode in ["120000", "160000", "100600"] {
        assert!(crate::candidate_checks::select_files(&[change("a.cpp", mode)], &p).is_err());
    }
    assert_eq!(
        crate::candidate_checks::select_files(&[change("--danger.cpp", "100755")], &p).unwrap(),
        vec!["--danger.cpp"]
    );
}

#[test]
fn confined_preflight_rejects_missing_links_fifo_and_collision() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("a")).unwrap();
    fs::write(f.0.join("a/b.cpp"), b"ok").unwrap();
    fs::write(f.0.join("a\\b.cpp"), b"different").unwrap();
    symlink("a", f.0.join("alias")).unwrap();
    symlink("a/b.cpp", f.0.join("link.cpp")).unwrap();
    let fifo = f.0.join("pipe.cpp");
    let c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let root = f.root();
    let p = policy();
    assert!(preflight_file_inputs(&p, &["a/b.cpp"], &root).is_ok());
    for name in [
        "missing.cpp",
        "alias/b.cpp",
        "link.cpp",
        "pipe.cpp",
        "a\\b.cpp",
    ] {
        assert!(preflight_file_inputs(&p, &[name], &root).is_err(), "{name}");
    }
    assert!(preflight_file_inputs(&p, &["a/b.cpp", "missing.cpp"], &root).is_err());
}

#[test]
fn list_budget_and_empty_legacy_selection() {
    let f = Fixture::new();
    let p = policy();
    let root = f.root();
    assert!(preflight_file_inputs(&p, &vec!["x"; 100_001], &root).is_err());
    let long = "a".repeat(4096);
    assert_eq!(
        preflight_file_inputs(&p, &vec![long.as_str(); 2048], &root).unwrap_err(),
        "file-input paths exceed 8 MiB"
    );
    let mut p = p;
    p["changes"] = json!([]);
    assert!(crate::candidate_checks::file_inputs::parse(&p).is_ok());
    assert!(super::super::integrity::files_for_policy(&p).is_ok());
    p["changes"] = json!([change("a.cpp", "100644")]);
    assert!(super::super::integrity::files_for_policy(&p).is_err());
}
