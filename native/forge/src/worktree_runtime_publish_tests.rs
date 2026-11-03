use super::*;
use crate::worktree_runtime::fixture::Fixture;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn captured(fixture: &Fixture) -> (Bin, Script, Vec<u8>, PathBuf) {
    let bytes = fixture.payload("python3", b"run()\n");
    let (_, destination) = fixture.linked("eligible", &bytes);
    let bin = Bin::open(&fixture.destination).unwrap();
    let stamp = Stamp::capture(&bin.dir.symlink_metadata("eligible").unwrap());
    let mut script = Script::open(&bin, OsStr::new("eligible"), &stamp).unwrap();
    let observed = script.bytes(&bin, OsStr::new("eligible")).unwrap();
    assert_eq!(observed, bytes);
    (bin, script, bytes, destination)
}
fn staging(bin: &Path) -> PathBuf {
    fs::read_dir(bin)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".runtime-relocation-")
        })
        .unwrap()
}
#[test]
fn changed_destination_before_exchange_is_preserved_and_owned_stage_is_removed() {
    let fixture = Fixture::new();
    let (bin, script, bytes, destination) = captured(&fixture);
    let error = replace_inner(
        &bin,
        OsStr::new("eligible"),
        &script,
        &bytes,
        b"new",
        &mut || Ok(()),
        &mut |phase| {
            if phase == "staged" {
                fs::rename(&destination, fixture.destination.join("bin/saved-original"))?;
                fs::write(&destination, b"foreign bytes")?;
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("changed"));
    assert_eq!(fs::read(destination).unwrap(), b"foreign bytes");
    assert_eq!(
        fs::read(fixture.source.join("bin/eligible")).unwrap(),
        bytes
    );
    assert!(!fs::read_dir(&bin.path).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .as_bytes()
        .starts_with(b".runtime-relocation-")));
}
#[test]
fn foreign_staging_replacement_is_not_cleaned_up() {
    let fixture = Fixture::new();
    let (bin, script, bytes, destination) = captured(&fixture);
    let mut retained = None;
    let error = replace_inner(
        &bin,
        OsStr::new("eligible"),
        &script,
        &bytes,
        b"new",
        &mut || Ok(()),
        &mut |phase| {
            if phase == "staged" {
                let slot = staging(&bin.path);
                fs::rename(&slot, fixture.destination.join("bin/saved-staging"))?;
                fs::write(&slot, b"foreign staging bytes")?;
                retained = Some(slot);
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("staging cleanup refused"));
    assert_eq!(
        fs::read(retained.unwrap()).unwrap(),
        b"foreign staging bytes"
    );
    assert_eq!(fs::read(destination).unwrap(), bytes);
}
#[test]
fn writer_racing_the_last_check_is_retained_in_the_displaced_slot() {
    let fixture = Fixture::new();
    let (bin, script, bytes, destination) = captured(&fixture);
    let error = replace_inner(
        &bin,
        OsStr::new("eligible"),
        &script,
        &bytes,
        b"new",
        &mut || Ok(()),
        &mut |phase| {
            if phase == "before_exchange" {
                fs::rename(&destination, fixture.destination.join("bin/saved-original"))?;
                fs::write(&destination, b"foreign race bytes")?;
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("publication may have occurred"));
    assert_eq!(fs::read(staging(&bin.path)).unwrap(), b"foreign race bytes");
    assert_eq!(fs::read(destination).unwrap(), b"new");
    assert_eq!(
        fs::read(fixture.source.join("bin/eligible")).unwrap(),
        bytes
    );
}
#[test]
fn post_exchange_foreign_edits_retain_the_original_slot() {
    let fixture = Fixture::new();
    let (bin, script, bytes, destination) = captured(&fixture);
    let error = replace_inner(
        &bin,
        OsStr::new("eligible"),
        &script,
        &bytes,
        b"new",
        &mut || Ok(()),
        &mut |phase| {
            if phase == "exchanged" {
                fs::write(&destination, b"foreign edit")?;
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("retain"));
    assert_eq!(fs::read(destination).unwrap(), b"foreign edit");
    assert_eq!(fs::read(staging(&bin.path)).unwrap(), bytes);
    assert_eq!(
        fs::read(fixture.source.join("bin/eligible")).unwrap(),
        bytes
    );
}
#[test]
fn changed_bin_binding_does_not_publish_through_the_replacement() {
    let fixture = Fixture::new();
    let (bin, script, bytes, _) = captured(&fixture);
    let saved = fixture.destination.join("saved-bin");
    let error = replace_inner(
        &bin,
        OsStr::new("eligible"),
        &script,
        &bytes,
        b"new",
        &mut || Ok(()),
        &mut |phase| {
            if phase == "staged" {
                fs::rename(&bin.path, &saved)?;
                fs::create_dir(&bin.path)?;
                fs::write(bin.path.join("eligible"), b"foreign replacement bin")?;
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("identity changed"));
    assert_eq!(
        fs::read(bin.path.join("eligible")).unwrap(),
        b"foreign replacement bin"
    );
    assert_eq!(fs::read(saved.join("eligible")).unwrap(), bytes);
}
