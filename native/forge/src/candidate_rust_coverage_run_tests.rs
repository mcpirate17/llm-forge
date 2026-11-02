use super::*;
#[test]
fn failed_build_retains_the_captured_compiler_diagnostic() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'compiler diagnostic on stdout'; exit 7"]);
    let message = output(command, "diagnostic fixture", 1024)
        .unwrap_err()
        .to_string();
    assert!(message.contains("exit status: 7"), "{message}");
    assert!(
        message.contains("compiler diagnostic on stdout"),
        "{message}"
    );
}
#[test]
fn tool_versions_parse_both_native_formats() {
    assert_eq!(
        llvm_version(b"rustc 1.98\nLLVM version: 22.1.8\n").unwrap(),
        (22, 1, 8)
    );
    assert_eq!(
        llvm_version(b"LLVM version 22.1.8-rust-1.98.0\n").unwrap(),
        (22, 1, 8)
    );
    assert!(llvm_version(b"unrelated\n").is_err());
}
#[test]
fn measured_test_output_accepts_cached_script_events_but_rejects_rebuilds() {
    assert!(reject_rebuilds(
        b"{\"reason\":\"compiler-artifact\",\"fresh\":true}\nordinary test output\n"
    )
    .is_ok());
    assert!(reject_rebuilds(b"{\"reason\":\"compiler-artifact\",\"fresh\":false}\n").is_err());
    assert!(reject_rebuilds(b"{\"reason\":\"build-script-executed\"}\n").is_ok());
}
#[test]
fn profiles_refuse_missing_symlinks_and_duplicate_oversized_objects() {
    let dir = tempfile::tempdir().unwrap();
    assert!(profiles(dir.path(), true).is_err());
    assert!(profiles(dir.path(), false).unwrap().is_empty());
    std::os::unix::fs::symlink("/dev/null", dir.path().join("bad.profraw")).unwrap();
    assert!(profiles(dir.path(), true).is_err());
    std::fs::remove_file(dir.path().join("bad.profraw")).unwrap();
    std::fs::write(dir.path().join("ok.profraw"), b"data").unwrap();
    let rows = profiles(dir.path(), true).unwrap();
    let bound = bind_binaries(&rows).unwrap();
    std::fs::write(dir.path().join("ok.profraw"), b"drift").unwrap();
    assert!(verify_binaries(&rows, &bound).is_err());
}
#[test]
fn profile_signatures_refuse_measured_build_execution() {
    let build = vec![
        PathBuf::from("123_0-100.profraw"),
        PathBuf::from("456_0-200.profraw"),
    ];
    assert!(reject_build_profiles(&build, &[PathBuf::from("789_0-300.profraw")]).is_ok());
    assert!(reject_build_profiles(&build, &[PathBuf::from("123_0-999.profraw")]).is_err());
    assert!(reject_build_profiles(&build, &[PathBuf::from("malformed.profraw")]).is_err());
}
