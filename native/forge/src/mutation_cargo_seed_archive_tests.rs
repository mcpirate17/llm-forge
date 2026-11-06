use super::*;
use std::os::unix::fs::PermissionsExt;
fn encoded(entries: &[(&str, tar::EntryType, &[u8])], pax: Option<(&str, &[u8])>) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    if let Some((key, value)) = pax {
        archive.append_pax_extensions([(key, value)]).unwrap();
    }
    for (path, kind, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*kind);
        header.set_size(bytes.len() as u64);
        header.set_mode(if path.ends_with("/run.sh") {
            0o755
        } else {
            0o644
        });
        header.set_mtime(1);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("outside").unwrap();
        }
        header.set_cksum();
        archive.append_data(&mut header, path, *bytes).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}
fn archive_fixture(bytes: &[u8]) -> Result<(tempfile::TempDir, Package)> {
    let temp = tempfile::tempdir()?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    let path = temp.path().join("registry/cache/crates-io");
    std::fs::create_dir_all(&path)?;
    // Explicitly correct only these newly owned fixture ancestor directories.
    for p in [
        temp.path().join("registry"),
        temp.path().join("registry/cache"),
        path.clone(),
    ] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = path.join("p-0.1.0.crate");
    std::fs::write(&file, bytes)?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    let package = Package {
        name: "p".into(),
        version: "0.1.0".into(),
        checksum: super::super::sha(bytes),
    };
    Ok((temp, package))
}
fn derive_bytes(bytes: &[u8], maximum: u64) -> Result<Expanded> {
    let (temp, package) = archive_fixture(bytes)?;
    derive_one(
        &io::root(temp.path())?,
        "crates-io",
        &package,
        &mut io::Budget::new(),
        &mut io::Budget::new(),
        maximum,
        None,
    )
}
#[test]
fn maintained_tar_reader_accepts_bounded_gnu_and_pax_paths() {
    let long = format!("p-0.1.0/src/{}/file.rs", "a".repeat(150));
    let bytes = encoded(
        &[
            ("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest"),
            (&long, tar::EntryType::Regular, b"source"),
        ],
        None,
    );
    let expanded = derive_bytes(&bytes, io::MAX_FILE).unwrap();
    assert!(expanded
        .entries
        .iter()
        .any(|e| e.path == long.trim_start_matches("p-0.1.0/")));
    let bytes = encoded(
        &[("p-0.1.0/placeholder", tar::EntryType::Regular, b"manifest")],
        Some(("path", b"p-0.1.0/Cargo.toml")),
    );
    assert!(derive_bytes(&bytes, io::MAX_FILE).is_ok());
}
#[test]
fn tar_path_escape_duplicate_wrong_root_and_sparse_metadata_refuse() {
    for path in [
        b"p-0.1.0/../outside".as_slice(),
        b"/outside",
        b"other/Cargo.toml",
    ] {
        let bytes = encoded(
            &[("p-0.1.0/placeholder", tar::EntryType::Regular, b"x")],
            Some(("path", path)),
        );
        assert!(derive_bytes(&bytes, io::MAX_FILE).is_err());
    }
    let duplicate = encoded(
        &[
            ("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"x"),
            ("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"x"),
        ],
        None,
    );
    assert!(derive_bytes(&duplicate, io::MAX_FILE).is_err());
    let sparse = encoded(
        &[("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"")],
        Some(("GNU.sparse.size", b"0")),
    );
    assert!(derive_bytes(&sparse, io::MAX_FILE).is_err());
}
#[test]
fn tar_links_special_types_and_reserved_cargo_marker_refuse() {
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::new(b'S'),
    ] {
        let bytes = encoded(&[("p-0.1.0/Cargo.toml", kind, b"")], None);
        assert!(derive_bytes(&bytes, io::MAX_FILE).is_err());
    }
    let marker = encoded(
        &[
            ("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest"),
            ("p-0.1.0/.cargo-ok", tar::EntryType::Regular, b"{\"v\":1}"),
        ],
        None,
    );
    assert!(derive_bytes(&marker, io::MAX_FILE).is_err());
}
#[test]
fn small_decompressed_limits_apply_below_file_and_extension_parsing() {
    let large = vec![0u8; 8192];
    let bytes = encoded(
        &[("p-0.1.0/Cargo.toml", tar::EntryType::Regular, &large)],
        None,
    );
    assert!(bytes.len() < 1024);
    assert!(derive_bytes(&bytes, 1024).is_err());
    let long = format!("p-0.1.0/{}", "a".repeat(8192));
    let bytes = encoded(&[(&long, tar::EntryType::Regular, b"")], None);
    assert!(derive_bytes(&bytes, 1024).is_err());
}
#[test]
fn gzip_integrity_and_trailing_streams_are_checked_even_with_matching_lock_digest() {
    let original = encoded(
        &[("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest")],
        None,
    );
    let mut corrupt = original.clone();
    let end = corrupt.len() - 1;
    corrupt[end] ^= 1;
    assert!(derive_bytes(&corrupt, io::MAX_FILE).is_err());
    let mut trailing = original;
    trailing.extend_from_slice(b"extra");
    assert!(derive_bytes(&trailing, io::MAX_FILE).is_err());
}

#[test]
fn eager_materialization_creates_verified_sources_without_restoring_tar_metadata() {
    use std::os::unix::fs::MetadataExt;
    let bytes = encoded(
        &[
            ("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest"),
            ("p-0.1.0/src/lib.rs", tar::EntryType::Regular, b"source"),
            ("p-0.1.0/run.sh", tar::EntryType::Regular, b"#!/bin/sh\n"),
        ],
        None,
    );
    let (temp, package) = archive_fixture(&bytes).unwrap();
    let source = io::root(temp.path()).unwrap();
    let expected = derive(&source, "crates-io", std::slice::from_ref(&package)).unwrap();
    let path = temp.path().join("stage");
    let target = io::new_root(&path, &[]).unwrap();
    materialize(&source, &target, "crates-io", &[package], &expected).unwrap();
    let package = path.join("registry/src/crates-io/p-0.1.0");
    assert_eq!(
        std::fs::read(package.join("src/lib.rs")).unwrap(),
        b"source"
    );
    assert_eq!(
        std::fs::read(package.join(".cargo-ok")).unwrap(),
        b"{\"v\":1}"
    );
    for (name, mode) in [("src", 0o700), ("src/lib.rs", 0o600), ("run.sh", 0o700)] {
        let meta = std::fs::metadata(package.join(name)).unwrap();
        assert_eq!(meta.mode() & 0o777, mode);
        assert_ne!(meta.mtime(), 1);
    }
    verify(&target, "crates-io", &expected).unwrap();
}
#[test]
fn failed_checksum_or_inventory_never_publishes_completion_marker() {
    for failure in ["checksum", "inventory"] {
        let bytes = encoded(
            &[("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest")],
            None,
        );
        let (temp, mut package) = archive_fixture(&bytes).unwrap();
        let source = io::root(temp.path()).unwrap();
        let mut expected = derive(&source, "crates-io", std::slice::from_ref(&package)).unwrap();
        if failure == "checksum" {
            package.checksum = "0".repeat(64);
        } else {
            expected[0].entries[0].sha256 = Some("0".repeat(64));
        }
        let path = temp.path().join("stage");
        let target = io::new_root(&path, &[]).unwrap();
        let error = materialize(&source, &target, "crates-io", &[package], &expected)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if failure == "checksum" {
                "checksum/identity drift"
            } else {
                "held expanded inventory"
            }),
            "{error}"
        );
        assert!(path
            .join("registry/src/crates-io/p-0.1.0/Cargo.toml")
            .is_file());
        assert_eq!(
            std::fs::symlink_metadata(path.join("registry/src/crates-io/p-0.1.0/.cargo-ok"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
#[test]
fn extraction_write_conflicts_and_symlinks_refuse_without_following_or_marking() {
    use std::os::unix::fs::symlink;
    let bytes = encoded(
        &[("p-0.1.0/Cargo.toml", tar::EntryType::Regular, b"manifest")],
        None,
    );
    for symlink_case in [false, true] {
        let (temp, package) = archive_fixture(&bytes).unwrap();
        let path = temp.path().join("stage");
        let target = io::new_root(&path, &[]).unwrap();
        let outside = temp.path().join("untouched");
        std::fs::write(&outside, b"original").unwrap();
        if symlink_case {
            symlink(&outside, path.join("Cargo.toml")).unwrap();
        } else {
            std::fs::write(path.join("Cargo.toml"), b"original").unwrap();
        }
        assert!(derive_one(
            &io::root(temp.path()).unwrap(),
            "crates-io",
            &package,
            &mut io::Budget::new(),
            &mut io::Budget::new(),
            io::MAX_FILE,
            Some(&target)
        )
        .is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        assert!(std::fs::symlink_metadata(path.join(".cargo-ok")).is_err());
    }
}
