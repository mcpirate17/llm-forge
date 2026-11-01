use super::*;
use std::{ffi::CString, fs, path::PathBuf};
struct Fixture {
    path: PathBuf,
    root: Dir,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::native_id::new("cargo-owner-test").unwrap());
        fs::create_dir(&path).unwrap();
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        Self { path, root }
    }
    fn write(&self, path: &str) {
        let destination = self.path.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, b"owned fixture\n").unwrap();
    }
    fn fifo(&self, path: &str) {
        let path = CString::new(self.path.join(path).as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove owned cargo-owner fixture");
    }
}

#[test]
fn nearest_root_and_lock_self_owners_are_sorted_deduplicated_and_serialized() {
    let f = Fixture::new();
    for path in [
        "Cargo.lock",
        "z.rs",
        "a/Cargo.lock",
        "a/src/lib.rs",
        "a/src/other.rs",
    ] {
        f.write(path);
    }
    let result = plan(
        &f.root,
        &[
            "z.rs",
            "a/src/lib.rs",
            "a/Cargo.lock",
            "a/src/other.rs",
            "z.rs",
        ],
    )
    .unwrap();
    assert_eq!(result.lockfiles, ["Cargo.lock", "a/Cargo.lock"]);
    assert_eq!(
        result
            .ownership
            .iter()
            .map(|entry| entry.selected.as_str())
            .collect::<Vec<_>>(),
        ["a/Cargo.lock", "a/src/lib.rs", "a/src/other.rs", "z.rs"]
    );
    assert_eq!(result.ownership[1].lockfile, "a/Cargo.lock");
    assert_eq!(result.ownership[3].lockfile, "Cargo.lock");
    let reversed = plan(
        &f.root,
        &["z.rs", "a/src/other.rs", "a/Cargo.lock", "a/src/lib.rs"],
    )
    .unwrap();
    assert_eq!(result, reversed);
    assert_eq!(
        serde_json::to_value(result).unwrap()["ownership"][0],
        serde_json::json!({"selected":"a/Cargo.lock","lockfile":"a/Cargo.lock"})
    );
}

#[test]
fn empty_is_empty_but_missing_deleted_and_partial_unowned_inputs_fail() {
    let f = Fixture::new();
    assert_eq!(
        plan(&f.root, &[]).unwrap(),
        AuditPlan {
            lockfiles: vec![],
            ownership: vec![]
        }
    );
    f.write("owned/Cargo.lock");
    f.write("owned/lib.rs");
    f.write("unowned.rs");
    assert!(plan(&f.root, &["owned/lib.rs", "unowned.rs"])
        .unwrap_err()
        .contains("no owning"));
    for missing in ["deleted.rs", "missing/Cargo.lock"] {
        assert!(plan(&f.root, &[missing])
            .unwrap_err()
            .contains("cargo-audit"));
    }
    f.write("Cargo.lock");
    assert!(
        plan(&f.root, &["deleted.rs"]).is_err(),
        "ancestor lock must not excuse missing selected input"
    );
}

#[test]
fn literal_backslash_is_distinct_from_directory_separator() {
    let f = Fixture::new();
    for path in ["Cargo.lock", "a\\b.rs", "a/b.rs", "a/Cargo.lock"] {
        f.write(path);
    }
    let result = plan(&f.root, &["a\\b.rs", "a/b.rs"]).unwrap();
    assert_eq!(
        result.ownership[0],
        Ownership {
            selected: "a/b.rs".into(),
            lockfile: "a/Cargo.lock".into()
        }
    );
    assert_eq!(
        result.ownership[1],
        Ownership {
            selected: "a\\b.rs".into(),
            lockfile: "Cargo.lock".into()
        }
    );
}

#[test]
fn symlink_parent_selected_leaf_and_lock_leaf_are_refused() {
    let f = Fixture::new();
    for path in ["Cargo.lock", "real/lib.rs", "target.lock"] {
        f.write(path);
    }
    std::os::unix::fs::symlink("real", f.path.join("alias")).unwrap();
    std::os::unix::fs::symlink("real/lib.rs", f.path.join("alias.rs")).unwrap();
    assert!(plan(&f.root, &["alias/lib.rs"]).is_err());
    assert!(plan(&f.root, &["alias.rs"])
        .unwrap_err()
        .contains("regular file"));
    std::os::unix::fs::symlink("../target.lock", f.path.join("real/Cargo.lock")).unwrap();
    assert!(plan(&f.root, &["real/lib.rs"])
        .unwrap_err()
        .contains("regular file"));
    // An outside link is rejected identically without opening its target.
    std::os::unix::fs::symlink("/outside-must-not-be-opened", f.path.join("outside.rs")).unwrap();
    assert!(plan(&f.root, &["outside.rs"])
        .unwrap_err()
        .contains("regular file"));
}

#[test]
fn fifo_and_directory_leaves_are_rejected_before_open() {
    let f = Fixture::new();
    f.write("Cargo.lock");
    f.fifo("fifo.rs");
    fs::create_dir(f.path.join("directory.rs")).unwrap();
    for path in ["fifo.rs", "directory.rs"] {
        let mut budget = Budget { probes: 0 };
        assert!(regular(&f.root, path, &mut budget)
            .unwrap_err()
            .contains("regular file"));
        assert_eq!(
            budget.probes, 1,
            "only nofollow metadata, never opening nonregular leaf"
        );
    }
    f.write("sub/lib.rs");
    f.fifo("sub/Cargo.lock");
    assert!(plan(&f.root, &["sub/lib.rs"])
        .unwrap_err()
        .contains("regular file"));
}

#[test]
fn all_cheap_path_count_byte_and_depth_bounds_precede_any_io() {
    let f = Fixture::new();
    for bad in ["", "/absolute", "a//b", "a/./b", "a/../b", "a/", "a\0b"] {
        assert!(plan(&f.root, &["missing.rs", bad])
            .unwrap_err()
            .contains("canonical"));
    }
    let long = "a".repeat(MAX_PATH + 1);
    assert!(plan(&f.root, &[&long]).unwrap_err().contains("canonical"));
    let deep = vec!["a"; MAX_DEPTH + 1].join("/");
    assert!(plan(&f.root, &[&deep]).unwrap_err().contains("depth"));
    assert!(plan(&f.root, &vec!["missing.rs"; MAX_INPUTS + 1])
        .unwrap_err()
        .contains("count"));
    let path = "a".repeat(MAX_PATH);
    assert!(plan(&f.root, &vec![path.as_str(); 2048])
        .unwrap_err()
        .contains("8 MiB"));
    assert!(admit(&[&vec!["a"; MAX_DEPTH].join("/")]).is_ok());
}

#[test]
fn work_and_distinct_lock_bounds_fail_instead_of_returning_partial_plan() {
    let f = Fixture::new();
    let mut budget = Budget { probes: MAX_PROBES };
    assert!(owner(&f.root, "missing.rs", &mut budget)
        .unwrap_err()
        .contains("probe limit"));
    let mut paths = Vec::new();
    for i in 0..=MAX_LOCKS {
        let path = format!("crate{i:03}/Cargo.lock");
        f.write(&path);
        paths.push(path);
    }
    let selected: Vec<&str> = paths.iter().map(String::as_str).collect();
    assert!(plan(&f.root, &selected[..MAX_LOCKS]).is_ok());
    assert!(plan(&f.root, &selected)
        .unwrap_err()
        .contains("distinct lockfile"));
}

#[test]
fn held_root_is_used_after_ambient_path_is_replaced() {
    let f = Fixture::new();
    f.write("snapshot/Cargo.lock");
    f.write("snapshot/lib.rs");
    let pinned = f.root.open_dir("snapshot").unwrap();
    fs::rename(f.path.join("snapshot"), f.path.join("held")).unwrap();
    fs::create_dir(f.path.join("snapshot")).unwrap();
    fs::write(f.path.join("snapshot/lib.rs"), b"decoy without lock").unwrap();
    assert_eq!(
        plan(&pinned, &["lib.rs"]).unwrap().lockfiles,
        ["Cargo.lock"]
    );
    assert_eq!(
        fs::read(f.path.join("snapshot/lib.rs")).unwrap(),
        b"decoy without lock"
    );
}

#[test]
fn unreadable_regular_input_and_lock_fail_without_partial_result() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    f.write("Cargo.lock");
    f.write("lib.rs");
    for relative in ["lib.rs", "Cargo.lock"] {
        let path = f.path.join(relative);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
        let result = plan(&f.root, &["lib.rs"]);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(result.unwrap_err().contains("open regular input/lockfile"));
    }
}

#[test]
fn compact_json_measurement_counts_escaping_utf8_and_exact_boundary() {
    let selected = "quote\"back\\line\nctrl\u{1}雪.rs";
    let value = AuditPlan {
        lockfiles: vec!["Cargo.lock".into()],
        ownership: vec![Ownership {
            selected: selected.into(),
            lockfile: "Cargo.lock".into(),
        }],
    };
    let expected = r#"{"lockfiles":["Cargo.lock"],"ownership":[{"selected":"quote\"back\\line\nctrl\u0001雪.rs","lockfile":"Cargo.lock"}]}"#;
    assert_eq!(serde_json::to_vec(&value).unwrap(), expected.as_bytes());
    assert_eq!(
        serialized_size(&value, expected.len()).unwrap(),
        expected.len()
    );
    assert!(serialized_size(&value, expected.len() - 1)
        .unwrap_err()
        .contains("serialized plan byte limit"));
    assert_eq!(serialized_size(&value, usize::MAX).unwrap(), expected.len());
    assert!(serialized_size(&value, 0).is_err());
    // Four raw bytes require 26 encoded bytes, not four: no filesystem scale needed.
    assert_eq!(serialized_size(&"\u{1}".repeat(4), 26).unwrap(), 26);
    assert!(serialized_size(&"\u{1}".repeat(4), 25).is_err());
}

#[test]
fn legal_control_quote_and_backslash_filename_survives_planning_and_json_bound() {
    let f = Fixture::new();
    let selected = "quote\"back\\ctrl\u{1}.rs";
    f.write("Cargo.lock");
    f.write(selected);
    let result = plan(&f.root, &[selected]).unwrap();
    assert_eq!(result.ownership[0].selected, selected);
    let encoded = serde_json::to_vec(&result).unwrap();
    assert!(encoded.len() > selected.len() + "Cargo.lock".len());
    assert_eq!(
        serialized_size(&result, MAX_JSON_BYTES).unwrap(),
        encoded.len()
    );
}
