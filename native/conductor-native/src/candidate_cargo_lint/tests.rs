use super::*;
use std::{fs, path::PathBuf};
pub(super) struct Fixture {
    pub(super) path: PathBuf,
    pub(super) root: Dir,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let path =
            std::env::temp_dir().join(crate::native_id::new("cargo-lint-core-test").unwrap());
        fs::create_dir(&path).unwrap();
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        Self { path, root }
    }
    pub(super) fn file(&self, path: &str) {
        let path = self.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = if path.file_name().is_some_and(|n| n == "Cargo.toml") {
            b"[package]\n".as_slice()
        } else {
            b"fixture\n".as_slice()
        };
        fs::write(path, bytes).unwrap();
    }
    fn fifo(&self, path: &str) {
        let path =
            std::ffi::CString::new(self.path.join(path).as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove owned lint fixture");
    }
}
pub(super) fn roster(tested: &[&str], extra: &str, globs: &[&str]) -> Roster {
    Roster::parse(
        format!(
            "[crates]\ntested={}\n{extra}\n[globs]\nmanifests={}\n",
            serde_json::to_string(tested).unwrap(),
            serde_json::to_string(globs).unwrap()
        )
        .as_bytes(),
    )
    .unwrap()
}

#[test]
fn root_nearest_deleted_and_directory_owners_are_sorted_and_deduplicated() {
    let f = Fixture::new();
    for path in ["Cargo.toml", "a/Cargo.toml", "a/src/lib.rs", "root.rs"] {
        f.file(path);
    }
    let r = roster(&[".", "a"], "", &["Cargo.toml", "*/Cargo.toml"]);
    let result = plan(
        &f.root,
        &r,
        &[
            "a/src/lib.rs",
            "root.rs",
            "a/deleted/sub/file.rs",
            "a/src/lib.rs",
            "a",
        ],
        Mode::Fmt,
    )
    .unwrap();
    assert_eq!(result.manifests, ["Cargo.toml", "a/Cargo.toml"]);
    assert_eq!(
        result
            .input_owners
            .iter()
            .map(|v| v.input.as_str())
            .collect::<Vec<_>>(),
        ["a", "a/deleted/sub/file.rs", "a/src/lib.rs", "root.rs"]
    );
    assert_eq!(result.input_owners[1].crate_path, "a");
    assert_eq!(result.input_owners[3].crate_path, ".");
    assert!(result.skips.is_empty());
}

#[test]
fn unclassified_whole_roster_fails_even_with_no_selected_files_and_orphans_fail() {
    let f = Fixture::new();
    for path in ["a/Cargo.toml", "b/Cargo.toml", "loose.rs"] {
        f.file(path);
    }
    let r = roster(&["a"], "", &["*/Cargo.toml"]);
    assert!(plan(&f.root, &r, &[], Mode::Fmt)
        .unwrap_err()
        .contains("unclassified"));
    let r = roster(&["a"], "excluded=['b']", &["*/Cargo.toml"]);
    assert!(plan(&f.root, &r, &["loose.rs"], Mode::Fmt)
        .unwrap_err()
        .contains("no owning Cargo.toml"));
    assert!(plan(&f.root, &r, &[], Mode::Fmt)
        .unwrap()
        .manifests
        .is_empty());
    // Excluded marks inventory coverage; it is not an implicit fmt exclusion.
    assert_eq!(
        plan(&f.root, &r, &["b/Cargo.toml"], Mode::Fmt)
            .unwrap()
            .manifests,
        ["b/Cargo.toml"]
    );
}

#[test]
fn fmt_unstyled_and_clippy_linted_prerequisites_are_visible_not_executed() {
    let f = Fixture::new();
    for path in ["a/Cargo.toml", "b/Cargo.toml", "held/Cargo.toml"] {
        f.file(path);
    }
    let mut text=String::from("[crates]\ntested=['a','b','held']\nlinted=['a','held']\nunstyled=['b']\n[globs]\nmanifests=['*/Cargo.toml']\n");
    text.push_str("[prerequisites.a]\nartifact='build/lib.a'\ncommand='must never run'\nreason='needs archive'\n[prerequisites.held]\ncommand='also never run'\nreason='held by operator'\n");
    let r = Roster::parse(text.as_bytes()).unwrap();
    let selected = ["held/Cargo.toml", "b/Cargo.toml", "a/Cargo.toml"];
    let fmt = plan(&f.root, &r, &selected, Mode::Fmt).unwrap();
    assert_eq!(fmt.manifests, ["a/Cargo.toml", "held/Cargo.toml"]);
    assert_eq!(fmt.skips[0].crate_path, "b");
    assert!(fmt.skips[0].reason.contains("unstyled"));
    let clippy = plan(&f.root, &r, &selected, Mode::Clippy).unwrap();
    assert!(clippy.manifests.is_empty());
    assert_eq!(clippy.skips.len(), 3);
    assert!(clippy.skips[0].reason.contains("must never run"));
    assert_eq!(clippy.skips[0].kind, SkipKind::Prerequisite);
    assert!(clippy.skips[1].reason.contains("not in crates.linted"));
    assert_eq!(clippy.skips[1].kind, SkipKind::NotLinted);
    assert!(clippy.skips[2].reason.contains("held by operator"));
    f.file("build/lib.a");
    assert_eq!(
        plan(&f.root, &r, &selected, Mode::Clippy)
            .unwrap()
            .manifests,
        ["a/Cargo.toml"]
    );
}

#[test]
fn component_globstar_zero_hidden_and_literal_prefix_do_not_walk_unused_trees() {
    let f = Fixture::new();
    for path in [
        "tree/Cargo.toml",
        "tree/.hidden/Cargo.toml",
        "tree/nested/Cargo.toml",
        "unused/special",
    ] {
        f.file(path);
    }
    std::os::unix::fs::symlink("/outside-never-read", f.path.join("unused/link")).unwrap();
    let r = roster(
        &["tree", "tree/.hidden", "tree/nested"],
        "",
        &["tree/**/Cargo.toml"],
    );
    let result = plan(&f.root, &r, &["tree/.hidden/gone.rs"], Mode::Fmt).unwrap();
    assert_eq!(result.manifests, ["tree/.hidden/Cargo.toml"]);
    let r = roster(&["tree/.hidden", "tree/nested"], "", &["tree/*/Cargo.toml"]);
    assert!(
        plan(&f.root, &r, &[], Mode::Fmt).is_ok(),
        "single star must not match root tree manifest"
    );
    f.file("tree/nested/deeper/Cargo.toml");
    assert!(
        plan(&f.root, &r, &[], Mode::Fmt).is_ok(),
        "single star cannot span two components"
    );
}

#[test]
fn selected_inventory_and_prerequisite_links_or_special_files_fail_closed() {
    let f = Fixture::new();
    f.file("a/Cargo.toml");
    f.file("a/lib.rs");
    std::os::unix::fs::symlink("a", f.path.join("alias")).unwrap();
    let r = roster(&["a"], "", &["a/Cargo.toml"]);
    assert!(plan(&f.root, &r, &["alias/lib.rs"], Mode::Fmt)
        .unwrap_err()
        .contains("symlink"));
    std::os::unix::fs::symlink("lib.rs", f.path.join("a/link.rs")).unwrap();
    assert!(plan(&f.root, &r, &["a/link.rs"], Mode::Fmt)
        .unwrap_err()
        .contains("symlink"));
    f.fifo("a/fifo.rs");
    assert!(plan(&f.root, &r, &["a/fifo.rs"], Mode::Fmt).is_err());
    let r = roster(&["a"], "", &["*/Cargo.toml"]);
    assert!(plan(&f.root, &r, &[], Mode::Fmt)
        .unwrap_err()
        .contains("symlink"));
    let text="[crates]\ntested=['a']\nlinted=['a']\n[globs]\nmanifests=['a/Cargo.toml']\n[prerequisites.a]\nartifact='artifact'\n";
    std::os::unix::fs::symlink("a/lib.rs", f.path.join("artifact")).unwrap();
    assert!(plan(
        &f.root,
        &Roster::parse(text.as_bytes()).unwrap(),
        &["a/lib.rs"],
        Mode::Clippy
    )
    .unwrap_err()
    .contains("symlink"));
}

#[test]
fn manifest_fifo_is_rejected_by_metadata_without_waiting_for_writer() {
    let f = Fixture::new();
    f.fifo("Cargo.toml");
    let r = roster(&["."], "", &["Cargo.toml"]);
    assert!(plan(&f.root, &r, &[], Mode::Fmt)
        .unwrap_err()
        .contains("nonregular"));
}

#[test]
fn strict_typed_roster_and_input_bounds_fail_before_inventory_io() {
    for bytes in [
        b"\xff".as_slice(),
        b"[crates]\nunknown=[]\n[globs]\nmanifests=['Cargo.toml']\n",
        b"[crates]\ntested=['../escape']\n[globs]\nmanifests=['Cargo.toml']\n",
        b"[crates]\ntested=['a','a']\n[globs]\nmanifests=['Cargo.toml']\n",
        b"[crates]\n[globs]\nmanifests=[]\n",
        b"[crates]\n[globs]\nmanifests=['**']\n",
    ] {
        assert!(Roster::parse(bytes).is_err());
    }
    assert!(Roster::parse(&vec![b' '; 1024 * 1024 + 1]).is_err());
    let f = Fixture::new();
    f.fifo("Cargo.toml");
    let r = roster(&["."], "", &["Cargo.toml"]);
    for bad in ["../x", "/x", "a//b", "a/./b", "a\0b"] {
        assert!(plan(&f.root, &r, &[bad], Mode::Fmt)
            .unwrap_err()
            .contains("path"));
    }
    assert!(plan(&f.root, &r, &vec!["x"; MAX_SELECTED + 1], Mode::Fmt)
        .unwrap_err()
        .contains("count"));
    let long = "a".repeat(MAX_PATH);
    assert!(plan(&f.root, &r, &vec![long.as_str(); 2048], Mode::Fmt)
        .unwrap_err()
        .contains("8 MiB"));
}

#[test]
fn inventory_work_and_entry_bounds_are_fail_loud_not_partial_success() {
    let f = Fixture::new();
    f.file("a/Cargo.toml");
    let r = roster(&["a"], "", &["*/Cargo.toml"]);
    let mut budget = Budget {
        probes: 1_000_000,
        entries: 0,
        work: 0,
        retained: 0,
    };
    assert!(inventory::collect(&f.root, &r.patterns, &mut budget)
        .unwrap_err()
        .contains("probes"));
    let mut budget = Budget {
        probes: 0,
        entries: 100_000,
        work: 0,
        retained: 0,
    };
    assert!(inventory::collect(&f.root, &r.patterns, &mut budget)
        .unwrap_err()
        .contains("entries"));
    let mut budget = Budget {
        probes: 0,
        entries: 0,
        work: 100_000_000,
        retained: 0,
    };
    assert!(inventory::collect(&f.root, &r.patterns, &mut budget)
        .unwrap_err()
        .contains("glob work"));
}

#[test]
fn read_optional_is_bounded_stable_binary_safe_and_missing_only_is_none() {
    let f = Fixture::new();
    f.file("dir/value");
    fs::write(f.path.join("dir/value"), [0xff, 0, 1]).unwrap();
    assert_eq!(
        read_optional(&f.root, "dir/value", 3).unwrap(),
        Some(vec![0xff, 0, 1])
    );
    assert!(read_optional(&f.root, "dir/value", 2)
        .unwrap_err()
        .contains("byte limit"));
    assert!(read_optional(&f.root, "dir/missing", 64).unwrap().is_none());
    assert!(read_optional(&f.root, "missing/child", 64)
        .unwrap()
        .is_none());
    for bad in [".", "..", "/absolute", "a//b"] {
        assert!(read_optional(&f.root, bad, 64).is_err());
    }
    assert!(read_optional(&f.root, "dir/value", 0).is_err());
    std::os::unix::fs::symlink("dir", f.path.join("alias")).unwrap();
    std::os::unix::fs::symlink("value", f.path.join("dir/link")).unwrap();
    assert!(read_optional(&f.root, "alias/value", 64)
        .unwrap_err()
        .contains("symlink"));
    assert!(read_optional(&f.root, "dir/link", 64)
        .unwrap_err()
        .contains("symlink"));
    f.fifo("fifo");
    assert!(read_optional(&f.root, "fifo", 64)
        .unwrap_err()
        .contains("regular"));
    assert!(read_optional(&f.root, "dir", 64)
        .unwrap_err()
        .contains("regular"));
}

#[test]
fn inventory_and_skip_kinds_preserve_scope_without_claiming_workspace_coverage() {
    let f = Fixture::new();
    for name in ["Cargo.toml", "a-b/Cargo.toml", "a/Cargo.toml"] {
        f.file(name);
    }
    let r = roster(
        &[".", "a-b", "a"],
        "unstyled=['a-b']\nlinted=['a']",
        &["Cargo.toml", "*/Cargo.toml"],
    );
    let fmt = plan(
        &f.root,
        &r,
        &[".", "a-b/Cargo.toml", "a/Cargo.toml"],
        Mode::Fmt,
    )
    .unwrap();
    assert_eq!(
        fmt.inventory,
        ["Cargo.toml", "a-b/Cargo.toml", "a/Cargo.toml"]
    );
    assert_eq!(fmt.skips[0].kind, SkipKind::Unstyled);
    let clippy = plan(&f.root, &r, &["Cargo.toml", "a-b/Cargo.toml"], Mode::Clippy).unwrap();
    assert!(clippy.skips.iter().all(|s| s.kind == SkipKind::NotLinted));
}

#[test]
fn glob_count_depth_and_non_utf8_inventory_fail_explicitly() {
    use std::os::unix::ffi::OsStringExt;
    let patterns = serde_json::to_string(&vec!["Cargo.toml"; 129]).unwrap();
    assert!(
        Roster::parse(format!("[crates]\n[globs]\nmanifests={patterns}\n").as_bytes()).is_err()
    );
    let deep = vec!["a"; 129].join("/");
    assert!(path_parts(&deep, false).is_err());
    let f = Fixture::new();
    fs::create_dir(f.path.join(std::ffi::OsString::from_vec(vec![0xff]))).unwrap();
    let r = roster(&[], "", &["*/Cargo.toml"]);
    assert!(plan(&f.root, &r, &[], Mode::Fmt)
        .unwrap_err()
        .contains("UTF-8"));
}
