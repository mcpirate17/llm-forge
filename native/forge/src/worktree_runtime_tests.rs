use super::*;
use fixture::{image, Fixture};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
};

#[test]
fn relocation_copies_on_write_preserves_modes_and_is_idempotent() {
    let fixture = Fixture::new();
    let original = fixture.payload("python3", b"from package import main\n");
    let (source, destination) = fixture.linked("pytest", &original);
    let original_inode = fs::metadata(&source).unwrap().ino();
    assert_eq!(
        relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(())).unwrap(),
        std::slice::from_ref(&destination)
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::metadata(&source).unwrap().ino(), original_inode);
    assert_ne!(fs::metadata(&destination).unwrap().ino(), original_inode);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o7777, 0o751);
    assert_eq!(
        fs::read(&destination).unwrap(),
        format!(
            "#!{}/bin/python3\nfrom package import main\n",
            fixture.destination.display()
        )
        .as_bytes()
    );
    let once = image(&fixture.root);
    assert!(
        relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(once, image(&fixture.root));

    let (source, destination) = fixture.linked("independent-mode", &original);
    fs::remove_file(&destination).unwrap();
    fs::write(&destination, &original).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o711)).unwrap();
    relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(())).unwrap();
    assert_eq!(fs::metadata(destination).unwrap().mode() & 0o7777, 0o711);
    assert_eq!(fs::read(source).unwrap(), original);
}
#[test]
fn exact_interpreters_and_crlf_rewrite_preserve_the_remaining_bytes() {
    let fixture = Fixture::new();
    for name in ["python", "python3.13", "python3.13.1", "python-any"] {
        fs::write(
            fixture.source.join("bin").join(name),
            b"interpreter name fixture\n",
        )
        .unwrap();
        fs::write(
            fixture.destination.join("bin").join(name),
            b"interpreter name fixture\n",
        )
        .unwrap();
    }
    let crlf = format!("#!{}/bin/python3\r\nrun()\r\n", fixture.source.display());
    let (_, crlf_destination) = fixture.linked("crlf", crlf.as_bytes());
    let body_with_nul = fixture.payload("python", b"prefix\0suffix\n");
    let (_, nul_body_destination) = fixture.linked("nul-body", &body_with_nul);
    fixture.linked("minor-version", &fixture.payload("python3.13", b"run()\n"));
    for (name, bytes) in [
        (
            "too-many-version-parts",
            fixture.payload("python3.13.1", b"run()\n"),
        ),
        ("non-python-name", fixture.payload("python-any", b"run()\n")),
        ("arguments", fixture.payload("python3 -O", b"run()\n")),
        ("foreign", b"#!/usr/bin/env python3\nrun()\n".to_vec()),
        (
            "no-newline",
            format!("#!{}/bin/python3", fixture.source.display()).into_bytes(),
        ),
    ] {
        fixture.linked(name, &bytes);
    }
    let count = relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()))
        .unwrap()
        .len();
    assert_eq!(count, 3);
    assert_eq!(
        fs::read(crlf_destination).unwrap(),
        format!("#!{}/bin/python3\nrun()\r\n", fixture.destination.display()).as_bytes()
    );
    assert_eq!(
        fs::read(nul_body_destination).unwrap(),
        [
            format!("#!{}/bin/python\n", fixture.destination.display()).as_bytes(),
            b"prefix\0suffix\n"
        ]
        .concat()
    );
    for name in [
        "too-many-version-parts",
        "non-python-name",
        "arguments",
        "foreign",
        "no-newline",
    ] {
        assert_eq!(
            fs::metadata(fixture.source.join("bin").join(name))
                .unwrap()
                .ino(),
            fs::metadata(fixture.destination.join("bin").join(name))
                .unwrap()
                .ino()
        );
    }
}
#[test]
fn relocation_skips_script_symlinks_binaries_missing_and_nonregular_entries() {
    let fixture = Fixture::new();
    let payload = fixture.payload("python3", b"run()\n");
    fixture.linked("eligible", &payload);
    fixture.linked("binary", b"\x7fELF\0not a script");
    symlink("python3", fixture.source.join("bin/source-link")).unwrap();
    fs::write(fixture.destination.join("bin/source-link"), &payload).unwrap();
    fs::write(fixture.source.join("bin/destination-link"), &payload).unwrap();
    symlink("python3", fixture.destination.join("bin/destination-link")).unwrap();
    fs::create_dir(fixture.source.join("bin/source-directory")).unwrap();
    fs::write(fixture.destination.join("bin/source-directory"), &payload).unwrap();
    fs::write(fixture.source.join("bin/destination-directory"), &payload).unwrap();
    fs::create_dir(fixture.destination.join("bin/destination-directory")).unwrap();
    fs::write(fixture.source.join("bin/missing"), &payload).unwrap();
    for (special, regular) in [
        (
            fixture.source.join("bin/source-fifo"),
            fixture.destination.join("bin/source-fifo"),
        ),
        (
            fixture.destination.join("bin/destination-fifo"),
            fixture.source.join("bin/destination-fifo"),
        ),
    ] {
        use std::os::unix::ffi::OsStrExt;
        let special = std::ffi::CString::new(special.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(special.as_ptr(), 0o600) }, 0);
        fs::write(regular, &payload).unwrap();
    }
    let before = image(&fixture.root);
    assert_eq!(
        relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(())).unwrap(),
        [fixture.destination.join("bin/eligible")]
    );
    let mut after = image(&fixture.root);
    let mut before = before;
    for map in [&mut before, &mut after] {
        map.remove(Path::new("destination-venv/bin/eligible"));
    }
    assert_eq!(before, after);
}
#[test]
fn interpreter_symlinks_use_metadata_only_and_dangling_or_special_targets_are_refused() {
    for kind in ["regular", "dangling", "directory"] {
        let fixture = Fixture::new();
        let payload = fixture.payload("python3", b"run()\n");
        let (source, destination) = fixture.linked("eligible", &payload);
        let target = fixture.root.join("interpreter-target");
        if kind == "regular" {
            fs::write(&target, b"metadata only, never executable\0\n").unwrap();
        }
        if kind == "directory" {
            fs::create_dir(&target).unwrap();
        }
        fs::remove_file(fixture.source.join("bin/python3")).unwrap();
        symlink(&target, fixture.source.join("bin/python3")).unwrap();
        let before = image(&fixture.root);
        let result =
            relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()));
        if kind == "regular" {
            assert_eq!(result.unwrap(), [destination]);
            assert_eq!(fs::read(&source).unwrap(), payload);
            assert_eq!(
                fs::read(target).unwrap(),
                b"metadata only, never executable\0\n"
            );
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("no Python executable"));
            assert_eq!(before, image(&fixture.root));
        }
    }
}
#[test]
fn relocation_refuses_same_incomplete_and_symlinked_venv_paths() {
    let fixture = Fixture::new();
    let before = image(&fixture.root);
    assert!(
        relocate_console_scripts(&fixture.source, &fixture.source, &mut || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("must differ")
    );
    assert!(relocate_console_scripts(&fixture.source, &fixture.root, &mut || Ok(())).is_err());
    let alias = fixture.root.join("source-alias");
    symlink(&fixture.source, &alias).unwrap();
    assert!(relocate_console_scripts(&alias, &fixture.destination, &mut || Ok(())).is_err());
    fs::remove_file(&alias).unwrap();
    assert_eq!(before, image(&fixture.root));
    for source_side in [false, true] {
        let selected = if source_side {
            &fixture.source
        } else {
            &fixture.destination
        };
        fs::rename(selected.join("bin"), selected.join("saved-bin")).unwrap();
        symlink(selected.join("saved-bin"), selected.join("bin")).unwrap();
        let before = image(&fixture.root);
        assert!(
            relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()))
                .is_err()
        );
        assert_eq!(before, image(&fixture.root));
        fs::remove_file(selected.join("bin")).unwrap();
        fs::rename(selected.join("saved-bin"), selected.join("bin")).unwrap();
    }
}
#[test]
fn oversized_matching_scripts_fail_before_writes_and_foreign_sparse_binaries_are_skipped() {
    let fixture = Fixture::new();
    let payload = fixture.payload("python3", b"run()\n");
    let (source, destination) = fixture.linked("eligible", &payload);
    fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(paths::MAX_FILE as u64 + 1)
        .unwrap();
    let inode = fs::metadata(&destination).unwrap().ino();
    let error = relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()))
        .unwrap_err();
    assert!(format!("{error:#}").contains("exceeds 64 MiB"));
    assert_eq!(fs::metadata(&source).unwrap().ino(), inode);
    assert_eq!(fs::metadata(&destination).unwrap().ino(), inode);
    fs::remove_file(source).unwrap();
    fs::remove_file(destination).unwrap();
    let (source, _) = fixture.linked("foreign-large", b"\x7fELF\0foreign\n");
    fs::OpenOptions::new()
        .write(true)
        .open(source)
        .unwrap()
        .set_len(paths::MAX_FILE as u64 + 1)
        .unwrap();
    assert!(
        relocate_console_scripts(&fixture.source, &fixture.destination, &mut || Ok(()))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn cancellation_is_reported_before_writes_and_after_completed_paths() {
    let fixture = Fixture::new();
    let payload = fixture.payload("python3", b"run()\n");
    let (source_a, destination_a) = fixture.linked("a", &payload);
    let (source_b, destination_b) = fixture.linked("b", &payload);
    let before = image(&fixture.root);
    assert!(relocate_console_scripts(
        &fixture.source,
        &fixture.destination,
        &mut || anyhow::bail!("cancelled test")
    )
    .is_err());
    assert_eq!(before, image(&fixture.root));
    let relocated_prefix = format!("#!{}/bin/python3\n", fixture.destination.display());
    let error = relocate_console_scripts(&fixture.source, &fixture.destination, &mut || {
        if fs::read(&destination_a)?.starts_with(relocated_prefix.as_bytes()) {
            anyhow::bail!("cancelled after first path");
        }
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("completed 1 earlier script(s)"));
    assert_eq!(fs::read(source_a).unwrap(), payload);
    assert_eq!(fs::read(source_b).unwrap(), payload);
    assert_eq!(fs::read(destination_b).unwrap(), payload);
}
