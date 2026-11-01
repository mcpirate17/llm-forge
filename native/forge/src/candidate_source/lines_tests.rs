use super::*;
use git2::Signature;
use std::{
    fs,
    path::{Path, PathBuf},
};
struct Fixture {
    repo: Repository,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::resource_id("candidate-lines-test").unwrap());
        fs::create_dir(&path).unwrap();
        let repo = Repository::init(&path).unwrap();
        Self { repo, path }
    }
    fn tree(&self, files: &[(&str, &[u8], i32)]) -> String {
        let mut tree = self.repo.treebuilder(None).unwrap();
        for (name, bytes, mode) in files {
            tree.insert(*name, self.repo.blob(bytes).unwrap(), *mode)
                .unwrap();
        }
        tree.write().unwrap().to_string()
    }
    fn capture(
        &self,
        base: &str,
        target: &str,
        status: &str,
        path: &str,
        old_path: Option<&str>,
    ) -> Change {
        let entry = |tree: &str, path: &str| {
            if tree == EMPTY_TREE {
                return None;
            }
            self.repo
                .find_tree(Oid::from_str(tree).unwrap())
                .unwrap()
                .get_path(Path::new(path))
                .ok()
                .map(|e| (e.id().to_string(), format!("{:06o}", e.filemode())))
        };
        let (old_oid, old_mode) =
            entry(base, old_path.unwrap_or(path)).unwrap_or(("0".repeat(40), "000000".into()));
        let (new_oid, new_mode) = entry(target, path).unwrap_or(("0".repeat(40), "000000".into()));
        Change {
            status: status.into(),
            path: path.into(),
            old_path: old_path.map(str::to_owned),
            old_mode,
            new_mode,
            old_oid,
            new_oid,
            classes: Vec::new(),
            risk: "normal".into(),
        }
    }
    fn staged(&self, name: &str, bytes: &[u8]) {
        fs::write(self.path.join(name), bytes).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
fn one(f: &Fixture, base: &str, target: &str, change: Change) -> Result<FileRanges> {
    let path = change.path.clone();
    let plan = checked_ranges(&f.repo, base, target, &[change], &[&path])?;
    Ok(plan.files.into_iter().next().unwrap())
}
#[test]
fn public_source_api_uses_staged_objects_despite_dirty_worktree_bytes() {
    let f = Fixture::new();
    f.staged("a.c", b"one\ntwo\n");
    let tree_oid = f.repo.index().unwrap().write_tree().unwrap();
    let tree = f.repo.find_tree(tree_oid).unwrap();
    let sig = Signature::now("fixture", "fixture@example.invalid").unwrap();
    f.repo
        .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    f.staged("a.c", b"one\nchanged\n");
    let source = Source::open(&f.path).unwrap();
    let candidate = source
        .resolve(super::super::Kind::Index, "HEAD", None)
        .unwrap();
    let index = fs::read(f.path.join(".git/index")).unwrap();
    let head = fs::read(f.path.join(".git/HEAD")).unwrap();
    fs::write(f.path.join("a.c"), b"dirty\nextra\nthird\n").unwrap();
    let result = source.changed_ranges(&candidate, &["a.c"]).unwrap();
    assert_eq!(result.files[0].ranges, vec![LineRange { start: 2, end: 2 }]);
    assert_eq!(
        result.files[0].blob_oid.as_deref(),
        Some(candidate.changes[0].new_oid.as_str())
    );
    assert_eq!(
        fs::read(f.path.join("a.c")).unwrap(),
        b"dirty\nextra\nthird\n"
    );
    assert_eq!(fs::read(f.path.join(".git/index")).unwrap(), index);
    assert_eq!(fs::read(f.path.join(".git/HEAD")).unwrap(), head);
}
#[test]
fn additions_no_final_lf_crlf_deletions_and_touching_changes_are_exact_intervals() {
    let f = Fixture::new();
    let added = f.tree(&[("new 雪.c", b"one\ntwo", 0o100644)]);
    let result = one(
        &f,
        EMPTY_TREE,
        &added,
        f.capture(EMPTY_TREE, &added, "A", "new 雪.c", None),
    )
    .unwrap();
    assert_eq!(result.ranges, vec![LineRange { start: 1, end: 2 }]);
    let base = f.tree(&[("a.c", b"one\r\ntwo\r\nthree\r\n", 0o100644)]);
    let target = f.tree(&[("a.c", b"one\r\nnew\r\nother\r\n", 0o100644)]);
    assert_eq!(
        one(
            &f,
            &base,
            &target,
            f.capture(&base, &target, "M", "a.c", None)
        )
        .unwrap()
        .ranges,
        vec![LineRange { start: 2, end: 3 }]
    );
    let target = f.tree(&[("a.c", b"one\r\n", 0o100644)]);
    let result = one(
        &f,
        &base,
        &target,
        f.capture(&base, &target, "M", "a.c", None),
    )
    .unwrap();
    assert_eq!(result.scope, RangeScope::NoChangedLines);
    assert!(result.ranges.is_empty());
    let empty = f.tree(&[]);
    let result = one(
        &f,
        &base,
        &empty,
        f.capture(&base, &empty, "D", "a.c", None),
    )
    .unwrap();
    assert_eq!(result.scope, RangeScope::Deleted);
    assert!(result.blob_oid.is_none());
}
#[test]
fn rename_copy_and_mode_only_use_verified_captured_source_endpoint() {
    let f = Fixture::new();
    let base = f.tree(&[("old.c", b"same\n", 0o100644)]);
    let renamed = f.tree(&[("new.c", b"same\n", 0o100644)]);
    let result = one(
        &f,
        &base,
        &renamed,
        f.capture(&base, &renamed, "R100", "new.c", Some("old.c")),
    )
    .unwrap();
    assert_eq!(result.scope, RangeScope::NoChangedLines);
    assert_eq!(result.old_path.as_deref(), Some("old.c"));
    let copied = f.tree(&[
        ("old.c", b"same\n", 0o100644),
        ("copy.c", b"same\n", 0o100644),
    ]);
    assert_eq!(
        one(
            &f,
            &base,
            &copied,
            f.capture(&base, &copied, "C100", "copy.c", Some("old.c"))
        )
        .unwrap()
        .scope,
        RangeScope::NoChangedLines
    );
    let mode = f.tree(&[("old.c", b"same\n", 0o100755)]);
    assert_eq!(
        one(
            &f,
            &base,
            &mode,
            f.capture(&base, &mode, "M", "old.c", None)
        )
        .unwrap()
        .scope,
        RangeScope::NoChangedLines
    );
    let renamed = f.tree(&[("new.c", b"different\n", 0o100644)]);
    assert_eq!(
        one(
            &f,
            &base,
            &renamed,
            f.capture(&base, &renamed, "R050", "new.c", Some("old.c"))
        )
        .unwrap()
        .ranges,
        vec![LineRange { start: 1, end: 1 }]
    );
}
#[test]
fn forged_path_mode_oid_old_path_and_status_are_refused() {
    let f = Fixture::new();
    let base = f.tree(&[
        ("old.c", b"one\n", 0o100644),
        ("other.c", b"one\n", 0o100644),
    ]);
    let target = f.tree(&[
        ("new.c", b"two\n", 0o100644),
        ("other.c", b"one\n", 0o100644),
    ]);
    let valid = f.capture(&base, &target, "R050", "new.c", Some("old.c"));
    for kind in 0..6 {
        let mut c = valid.clone();
        match kind {
            0 => c.new_oid = c.old_oid.clone(),
            1 => c.new_mode = "100755".into(),
            2 => c.path = "../new.c".into(),
            3 => c.old_path = Some("other.c".into()),
            4 => c.status = "R999".into(),
            _ => c.old_path = None,
        }
        assert!(one(&f, &base, &target, c).is_err());
    }
    assert!(checked_ranges(
        &f.repo,
        &"f".repeat(40),
        &target,
        std::slice::from_ref(&valid),
        &["new.c"]
    )
    .is_err());
    assert!(checked_ranges(
        &f.repo,
        &base,
        &target,
        std::slice::from_ref(&valid),
        &["unknown.c"]
    )
    .is_err());
    assert!(checked_ranges(&f.repo, &base, &target, &[valid.clone(), valid], &[]).is_err());
}
#[test]
fn binary_invalid_utf8_and_nonregular_git_sources_fail_without_worktree_fallback() {
    let f = Fixture::new();
    for bytes in [b"a\0b".as_slice(), b"\xff"] {
        let target = f.tree(&[("a.c", bytes, 0o100644)]);
        assert!(one(
            &f,
            EMPTY_TREE,
            &target,
            f.capture(EMPTY_TREE, &target, "A", "a.c", None)
        )
        .is_err());
    }
    let link = f.tree(&[("a.c", b"target", 0o120000)]);
    assert!(one(
        &f,
        EMPTY_TREE,
        &link,
        f.capture(EMPTY_TREE, &link, "A", "a.c", None)
    )
    .unwrap_err()
    .to_string()
    .contains("regular Git"));
}
#[test]
fn expensive_line_work_and_blob_read_admission_fail_loud_before_unbounded_diff() {
    let f = Fixture::new();
    let old = b"old\n".repeat(10_000);
    let new = b"new\n".repeat(10_000);
    let base = f.tree(&[("a.c", &old, 0o100644)]);
    let target = f.tree(&[("a.c", &new, 0o100644)]);
    assert!(one(
        &f,
        &base,
        &target,
        f.capture(&base, &target, "M", "a.c", None)
    )
    .unwrap_err()
    .to_string()
    .contains("work exceeds"));
    let oversized = vec![b'a'; MAX_BLOB + 1];
    let target = f.tree(&[("a.c", &oversized, 0o100644)]);
    assert!(one(
        &f,
        EMPTY_TREE,
        &target,
        f.capture(EMPTY_TREE, &target, "A", "a.c", None)
    )
    .unwrap_err()
    .to_string()
    .contains("8MiB"));
    let oid = f.repo.blob(&vec![b'a'; MAX_BLOB]).unwrap();
    let mut budget = Budget::new();
    for _ in 0..4 {
        budget.blob(&f.repo, oid).unwrap();
    }
    assert!(budget
        .blob(&f.repo, oid)
        .unwrap_err()
        .to_string()
        .contains("32MiB"));
}
#[test]
fn hunk_range_and_encoded_output_caps_are_compositional() {
    let f = Fixture::new();
    let oid = f.repo.blob(b"one\n").unwrap();
    let blob = f.repo.find_blob(oid).unwrap();
    let mut budget = Budget::new();
    budget.hunks = MAX_HUNKS;
    assert!(diff(&f.repo, None, &blob, 1, &mut budget)
        .unwrap_err()
        .to_string()
        .contains("hunks"));
    let mut budget = Budget::new();
    budget.ranges = MAX_HUNKS;
    assert!(diff(&f.repo, None, &blob, 1, &mut budget)
        .unwrap_err()
        .to_string()
        .contains("ranges"));
    let value = "\u{1}\"雪";
    let expected = r#""\u0001\"雪""#;
    assert_eq!(
        encoded_size(&value, expected.len()).unwrap(),
        expected.len()
    );
    assert!(encoded_size(&value, expected.len() - 1).is_err());
}

#[test]
fn count_path_tree_type_and_lookup_budgets_precede_source_projection() {
    let f = Fixture::new();
    assert!(checked_ranges(
        &f.repo,
        EMPTY_TREE,
        EMPTY_TREE,
        &[],
        &vec!["a"; MAX_FILES + 1]
    )
    .unwrap_err()
    .to_string()
    .contains("count"));
    let path = "a".repeat(4096);
    assert!(checked_ranges(
        &f.repo,
        EMPTY_TREE,
        EMPTY_TREE,
        &[],
        &vec![path.as_str(); 2048]
    )
    .unwrap_err()
    .to_string()
    .contains("8MiB"));
    let blob = f.repo.blob(b"not a tree").unwrap();
    assert!(
        checked_ranges(&f.repo, &blob.to_string(), EMPTY_TREE, &[], &[])
            .unwrap_err()
            .to_string()
            .contains("tree object")
    );
    let tree = f.tree(&[("a.c", b"a\n", 0o100644)]);
    let mut cache = Trees::new(&f.repo);
    cache.lookups = 1_000_000;
    assert!(cache
        .lookup(Oid::from_str(&tree).unwrap(), "a.c")
        .err()
        .unwrap()
        .to_string()
        .contains("lookups"));
}

#[test]
fn deleted_binary_is_explicitly_skipped_and_gitlink_cannot_masquerade_as_source() {
    let f = Fixture::new();
    let base = f.tree(&[("binary.c", b"x\0y", 0o100644)]);
    let empty = f.tree(&[]);
    assert_eq!(
        one(
            &f,
            &base,
            &empty,
            f.capture(&base, &empty, "D", "binary.c", None)
        )
        .unwrap()
        .scope,
        RangeScope::Deleted
    );
    let tree = f.repo.find_tree(Oid::from_str(&empty).unwrap()).unwrap();
    let sig = Signature::now("fixture", "fixture@example.invalid").unwrap();
    let commit = f
        .repo
        .commit(None, &sig, &sig, "gitlink fixture", &tree, &[])
        .unwrap();
    let mut builder = f.repo.treebuilder(None).unwrap();
    builder.insert("linked.c", commit, 0o160000).unwrap();
    let target = builder.write().unwrap().to_string();
    let mut c = f.capture(EMPTY_TREE, &target, "A", "linked.c", None);
    c.new_mode = "100644".into();
    assert!(one(&f, EMPTY_TREE, &target, c)
        .unwrap_err()
        .to_string()
        .contains("mode/OID"));
}

#[test]
fn newline_heavy_addition_is_refused_before_libgit2_line_array_amplification() {
    let f = Fixture::new();
    let bytes = b"\n".repeat(100_001);
    let target = f.tree(&[("a.c", &bytes, 0o100644)]);
    assert!(one(
        &f,
        EMPTY_TREE,
        &target,
        f.capture(EMPTY_TREE, &target, "A", "a.c", None)
    )
    .unwrap_err()
    .to_string()
    .contains("line count"));
}
