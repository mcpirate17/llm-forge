#![cfg(target_os = "linux")]
#[path = "../../forge/tests/support/runtime_relocation.rs"]
mod fixture;
use fixture::{image, Fixture};
use serde_json::Value;
use std::{
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{symlink, MetadataExt},
    },
    process::{Command, Output},
};
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command.arg("worktree-runtime");
    command
}
fn json(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn actual_binary_repairs_hardlinked_launchers_and_preserves_legacy_text_idempotence() {
    let fixture = Fixture::new();
    let original = fixture.payload("python3", b"from package import main\n");
    let (source, destination) = fixture.linked("pytest", &original);
    let inode = fs::metadata(&source).unwrap().ino();
    let output = command()
        .arg(&fixture.source)
        .arg(&fixture.destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"relocated 1 Python console script(s)\n");
    assert_eq!(fs::read(source).unwrap(), original);
    assert_ne!(fs::metadata(&destination).unwrap().ino(), inode);
    assert_eq!(
        fs::read(destination).unwrap(),
        format!(
            "#!{}/bin/python3\nfrom package import main\n",
            fixture.destination.display()
        )
        .as_bytes()
    );
    let once = image(&fixture.root);
    let again = command()
        .arg(&fixture.source)
        .arg(&fixture.destination)
        .output()
        .unwrap();
    assert!(again.status.success());
    assert_eq!(again.stdout, b"relocated 0 Python console script(s)\n");
    assert_eq!(once, image(&fixture.root));
}
#[test]
fn actual_binary_uses_only_selected_interpreter_metadata_and_preserves_crlf_rest() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source.join("bin/python3")).unwrap();
    let interpreter = fixture.root.join("metadata-only-interpreter");
    fs::write(&interpreter, b"not executable\0fixture\n").unwrap();
    symlink(&interpreter, fixture.source.join("bin/python3")).unwrap();
    let payload = format!("#!{}/bin/python3\r\nbody\r\n", fixture.source.display());
    let (_, destination) = fixture.linked("tool", payload.as_bytes());
    let report = json(
        &command()
            .current_dir(&fixture.root)
            .args(["source-venv", "destination-venv", "--json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(report["interface"], "forge.worktree-runtime");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["relocated_count"], 1);
    assert_eq!(
        fs::read(destination).unwrap(),
        format!("#!{}/bin/python3\nbody\r\n", fixture.destination.display()).as_bytes()
    );
    assert_eq!(fs::read(interpreter).unwrap(), b"not executable\0fixture\n");
}
#[test]
fn actual_binary_refuses_unsafe_operands_and_leaves_foreign_entries_untouched() {
    let fixture = Fixture::new();
    let payload = fixture.payload("python3", b"run()\n");
    fixture.linked("eligible", &payload);
    let before = image(&fixture.root);
    let same = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.source)
            .arg("--json")
            .output()
            .unwrap(),
        2,
    );
    assert!(same["error"].as_str().unwrap().contains("must differ"));
    assert_eq!(before, image(&fixture.root));
    fs::rename(
        fixture.destination.join("bin"),
        fixture.destination.join("saved-bin"),
    )
    .unwrap();
    symlink(fixture.source.join("bin"), fixture.destination.join("bin")).unwrap();
    let before = image(&fixture.root);
    let linked = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.destination)
            .arg("--json")
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(linked["outcome"], "error");
    assert_eq!(before, image(&fixture.root));
    let no_operands = command().output().unwrap();
    assert_eq!(no_operands.status.code(), Some(2));
}
#[test]
fn actual_binary_keeps_foreign_interpreters_scripts_and_symlinks_unchanged() {
    let fixture = Fixture::new();
    let (_, destination) = fixture.linked("foreign", b"#!/usr/bin/env python3\nrun()\n");
    let payload = fixture.payload("python3", b"run()\n");
    fs::write(fixture.source.join("bin/destination-link"), &payload).unwrap();
    symlink(
        &destination,
        fixture.destination.join("bin/destination-link"),
    )
    .unwrap();
    let before = image(&fixture.root);
    let report = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.destination)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(report["relocated_count"], 0);
    assert_eq!(before, image(&fixture.root));
}
#[test]
fn actual_binary_json_preserves_non_utf8_destination_names_without_loss() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let name = std::ffi::OsString::from_vec(b"tool-\xff".to_vec());
    let payload = fixture.payload("python3", b"run()\n");
    let source = fixture.source.join("bin").join(&name);
    let destination = fixture.destination.join("bin").join(&name);
    fs::write(&source, &payload).unwrap();
    fs::hard_link(&source, &destination).unwrap();
    let report = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.destination)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(report["relocated_count"], 1);
    assert_eq!(report["relocated"][0]["path"], Value::Null);
    assert_eq!(
        report["relocated"][0]["path_bytes"],
        serde_json::json!(destination.as_os_str().as_bytes())
    );
    assert_eq!(fs::read(source).unwrap(), payload);
}
#[test]
fn actual_binary_refuses_a_cooperative_writer_lease_then_succeeds_after_release() {
    use std::os::fd::AsRawFd;
    let fixture = Fixture::new();
    fixture.linked("eligible", &fixture.payload("python3", b"run()\n"));
    let held = fs::File::open(fixture.source.join("bin")).unwrap();
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let before = image(&fixture.root);
    let busy = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.destination)
            .arg("--json")
            .output()
            .unwrap(),
        2,
    );
    assert!(busy["error"]
        .as_str()
        .unwrap()
        .contains("busy or cannot be locked"));
    assert_eq!(before, image(&fixture.root));
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    let retried = json(
        &command()
            .arg(&fixture.source)
            .arg(&fixture.destination)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(retried["relocated_count"], 1);
}
