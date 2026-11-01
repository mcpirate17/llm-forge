use super::*;
use git2::{RepositoryInitOptions, Signature};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    repository: Repository,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-candidate-source-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(&path, &options).unwrap();
        Self { repository, path }
    }
    fn stage(&self, path: &str, content: &str) {
        let file = self.path.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
        let mut index = self.repository.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn tree(&self) -> Oid {
        self.repository.index().unwrap().write_tree().unwrap()
    }
    fn commit(&self, parents: &[Oid], update: Option<&str>) -> Oid {
        let tree = self.repository.find_tree(self.tree()).unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| self.repository.find_commit(*oid).unwrap())
            .collect();
        let refs: Vec<_> = parents.iter().collect();
        let signature = Signature::now("Fixture", "fixture@example.invalid").unwrap();
        self.repository
            .commit(update, &signature, &signature, "fixture", &tree, &refs)
            .unwrap()
    }
    fn source(&self) -> Source {
        Source::open(&self.path).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).unwrap();
    }
}

#[test]
fn staged_candidate_uses_index_not_dirty_files_and_keeps_two_bases() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "base\n");
    let first = fixture.commit(&[], Some("HEAD"));
    fixture.stage("a.txt", "committed\n");
    let head = fixture.commit(&[first], Some("HEAD"));
    fixture
        .repository
        .reference("refs/remotes/origin/main", first, true, "fixture")
        .unwrap();
    fixture.stage("a.txt", "staged\n");
    let staged_tree = fixture.tree();
    std::fs::write(fixture.path.join("a.txt"), "dirty unreviewed\n").unwrap();
    let source = fixture.source();
    let candidate = source.resolve(Kind::Index, "ignored", None).unwrap();
    assert_eq!(candidate.tree_oid, staged_tree.to_string());
    assert_eq!(candidate.base_commit_oid, Some(head.to_string()));
    assert_eq!(candidate.integration_base_oid, Some(first.to_string()));
    assert_eq!(candidate.waiver_base(), Some(first.to_string().as_str()));
    assert!(candidate.commit_oid.is_none() && candidate.target_ref.is_none());
    assert_eq!(candidate.changes.len(), 1);
    assert_eq!(candidate.changes[0].status, "M");
    assert!(candidate.changes[0].classes.is_empty());
    assert_eq!(candidate.changes[0].risk, "normal");
    assert_eq!(source.root(), fixture.path.canonicalize().unwrap());
    assert_eq!(
        source.common_dir(),
        fixture.path.join(".git").canonicalize().unwrap()
    );
}

#[test]
fn unborn_and_root_commit_candidates_have_empty_review_base() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "first\n");
    let source = fixture.source();
    let initial = source.resolve(Kind::Index, "HEAD", None).unwrap();
    assert_eq!(initial.base_tree_oid, EMPTY_TREE);
    assert!(initial.base_commit_oid.is_none() && initial.waiver_base().is_none());
    assert_eq!(initial.changes[0].status, "A");
    assert_eq!(initial.changes[0].old_oid, "0".repeat(40));
    assert_eq!(initial.changes[0].old_mode, "000000");
    let first = fixture.commit(&[], Some("HEAD"));
    let root = source.resolve(Kind::Commit, "HEAD", None).unwrap();
    assert_eq!(root.commit_oid, Some(first.to_string()));
    assert_eq!(root.base_tree_oid, EMPTY_TREE);
    assert!(root.waiver_base().is_none());
    assert!(source.list_tree(EMPTY_TREE).unwrap().is_empty());
}

#[test]
fn merges_ranges_and_unrelated_explicit_bases_preserve_waiver_safety() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "first\n");
    let first = fixture.commit(&[], Some("HEAD"));
    fixture.stage("a.txt", "left\n");
    let left = fixture.commit(&[first], None);
    fixture.stage("a.txt", "right\n");
    let right = fixture.commit(&[first], None);
    let merge = fixture.commit(&[left, right], None);
    let orphan = fixture.commit(&[], None);
    let source = fixture.source();
    assert!(source
        .resolve(Kind::Commit, &merge.to_string(), None)
        .is_err());
    let explicit = source
        .resolve(Kind::Commit, &merge.to_string(), Some(&left.to_string()))
        .unwrap();
    assert_eq!(explicit.waiver_base(), Some(left.to_string().as_str()));
    let range = source
        .resolve(Kind::Range, &right.to_string(), Some(&left.to_string()))
        .unwrap();
    assert_eq!(range.base_commit_oid, Some(first.to_string()));
    assert_eq!(range.integration_base_oid, Some(first.to_string()));
    assert!(source
        .resolve(Kind::Range, &right.to_string(), None)
        .is_err());
    assert!(source
        .resolve(Kind::Range, &right.to_string(), Some(&orphan.to_string()))
        .is_err());
    let unrelated = source
        .resolve(Kind::Commit, &right.to_string(), Some(&orphan.to_string()))
        .unwrap();
    assert_eq!(unrelated.base_commit_oid, Some(orphan.to_string()));
    assert!(unrelated.waiver_base().is_none());
    assert!(unrelated
        .integration_base_detail
        .contains("not an ancestor"));
    let staged = source
        .resolve(Kind::Index, "HEAD", Some(&orphan.to_string()))
        .unwrap();
    assert!(staged.waiver_base().is_none());
}

#[test]
fn ambiguous_short_refs_require_an_exact_namespace_and_tags_peel() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "first\n");
    let first = fixture.commit(&[], Some("HEAD"));
    fixture
        .repository
        .reference("refs/heads/shared", first, false, "fixture")
        .unwrap();
    fixture
        .repository
        .tag_lightweight(
            "shared",
            &fixture.repository.find_object(first, None).unwrap(),
            false,
        )
        .unwrap();
    let source = fixture.source();
    assert!(source
        .resolve_commit("shared")
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    for reference in ["refs/heads/shared", "refs/tags/shared", "HEAD", "HEAD~0"] {
        assert_eq!(source.resolve_commit(reference).unwrap(), first.to_string());
    }
    for reference in ["", "--all"] {
        assert!(source.resolve_commit(reference).is_err());
    }
}

#[test]
fn renames_and_copies_keep_raw_scores_and_paths() {
    let fixture = Fixture::new();
    let text = (0..100)
        .map(|n| format!("a distinct source line {n}\n"))
        .collect::<String>();
    fixture.stage("old.txt", &text);
    fixture.stage("copy-source.txt", &text.replace("source", "copy"));
    let first = fixture.commit(&[], Some("HEAD"));
    let mut index = fixture.repository.index().unwrap();
    index.remove_path(Path::new("old.txt")).unwrap();
    index.write().unwrap();
    fixture.stage("renamed.txt", &text);
    fixture.stage("copied.txt", &text.replace("source", "copy"));
    fixture.stage("copy-source.txt", "rewritten content\n");
    let candidate = fixture
        .source()
        .resolve(Kind::Index, "HEAD", Some(&first.to_string()))
        .unwrap();
    let renamed = candidate
        .changes
        .iter()
        .find(|c| c.path == "renamed.txt")
        .unwrap();
    assert_eq!(renamed.status, "R100");
    assert_eq!(renamed.old_path.as_deref(), Some("old.txt"));
    let copied = candidate
        .changes
        .iter()
        .find(|c| c.path == "copied.txt")
        .unwrap();
    assert_eq!(copied.status, "C100");
    assert_eq!(copied.old_path.as_deref(), Some("copy-source.txt"));
}

#[test]
fn tree_listing_keeps_modes_gitlinks_and_rejects_invalid_paths() {
    let fixture = Fixture::new();
    let blob = fixture.repository.blob(b"hello").unwrap();
    let mut builder = fixture.repository.treebuilder(None).unwrap();
    builder.insert("exec", blob, 0o100755).unwrap();
    builder.insert("link", blob, 0o120000).unwrap();
    // Gitlinks need not exist in this repository's object database.
    builder
        .insert("module", Oid::from_str(&"a".repeat(40)).unwrap(), 0o160000)
        .unwrap();
    let source = fixture.source();
    let entries = source
        .list_tree(&builder.write().unwrap().to_string())
        .unwrap();
    assert_eq!(
        entries.iter().map(|e| e.mode.as_str()).collect::<Vec<_>>(),
        vec!["100755", "120000", "160000"]
    );
    assert_eq!(entries[0].size, Some(5));
    assert_eq!(entries[2].object_type, "commit");
    assert!(entries[2].size.is_none());
    builder.insert(&b"bad-\xff"[..], blob, 0o100644).unwrap();
    assert!(source
        .list_tree(&builder.write().unwrap().to_string())
        .is_err());
    for path in ["../escape", "a/./b", "/absolute", "a//b"] {
        assert!(safe_path(path.as_bytes()).is_err());
    }
    assert!(safe_path(&vec![b'a'; MAX_PATH + 1]).is_err());
}

#[test]
fn integration_configuration_is_loaded_from_explicit_repository() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "base\n");
    let base = fixture.commit(&[], Some("HEAD"));
    fixture.stage("a.txt", "head\n");
    let head = fixture.commit(&[base], Some("HEAD"));
    fixture
        .repository
        .reference("refs/heads/integration", base, false, "fixture")
        .unwrap();
    std::fs::write(
        fixture.path.join("pyproject.toml"),
        "[tool.conductor]\nintegration_branch = 'integration'\n",
    )
    .unwrap();
    let candidate = fixture.source().resolve(Kind::Index, "HEAD", None).unwrap();
    assert_eq!(candidate.base_commit_oid, Some(head.to_string()));
    assert_eq!(candidate.integration_base_oid, Some(base.to_string()));
    assert_eq!(
        candidate.integration_base_detail,
        "merge base with integration"
    );
}

#[test]
fn deletions_and_executable_changes_keep_exact_modes_and_null_old_path() {
    let fixture = Fixture::new();
    fixture.stage("mode.txt", "mode\n");
    fixture.stage("remove.txt", "remove\n");
    fixture.commit(&[], Some("HEAD"));
    let mut index = fixture.repository.index().unwrap();
    let mut entry = index.get_path(Path::new("mode.txt"), 0).unwrap();
    entry.mode = 0o100755;
    index.add(&entry).unwrap();
    index.remove_path(Path::new("remove.txt")).unwrap();
    index.write().unwrap();
    let candidate = fixture.source().resolve(Kind::Index, "HEAD", None).unwrap();
    let mode = candidate
        .changes
        .iter()
        .find(|c| c.path == "mode.txt")
        .unwrap();
    assert_eq!((&*mode.old_mode, &*mode.new_mode), ("100644", "100755"));
    assert!(mode.old_path.is_none());
    let deleted = candidate
        .changes
        .iter()
        .find(|c| c.path == "remove.txt")
        .unwrap();
    assert!(deleted.deleted());
    assert_eq!(deleted.status, "D");
    assert_eq!(deleted.new_oid, "0".repeat(40));
    assert!(deleted.old_path.is_none());
}

#[test]
fn compressed_index_paths_resolve_and_excessive_count_is_refused_early() {
    let fixture = Fixture::new();
    fixture.stage("common-prefix/a.txt", "a\n");
    fixture.stage("common-prefix/b.txt", "b\n");
    let mut index = fixture.repository.index().unwrap();
    index.set_version(4).unwrap();
    index.write().unwrap();
    let source = fixture.source();
    assert_eq!(
        source
            .resolve(Kind::Index, "HEAD", None)
            .unwrap()
            .changes
            .len(),
        2
    );
    let mut raw = vec![0u8; 32];
    raw[..4].copy_from_slice(b"DIRC");
    raw[4..8].copy_from_slice(&2u32.to_be_bytes());
    raw[8..12].copy_from_slice(&((MAX_ENTRIES + 1) as u32).to_be_bytes());
    std::fs::write(fixture.path.join(".git/index"), raw).unwrap();
    assert!(source
        .resolve(Kind::Index, "HEAD", None)
        .unwrap_err()
        .to_string()
        .contains("excessive entry count"));
}

#[test]
fn admitted_index_bytes_survive_original_replacement_and_alternate_index_is_independent() {
    let fixture = Fixture::new();
    fixture.stage("a.txt", "admitted\n");
    let admitted_tree = fixture.tree();
    let original = fixture.path.join(".git/index");
    let admitted = crate::bounded_file::read(&original, MAX_OUTPUT).unwrap();
    let alternate = fixture.path.join("alternate-index");
    std::fs::write(&alternate, &admitted).unwrap();
    fixture.stage("a.txt", "different current index\n");
    assert_ne!(fixture.tree(), admitted_tree);
    let mut selected = resolve::read_index(&alternate).unwrap();
    assert_eq!(
        selected.write_tree_to(&fixture.repository).unwrap(),
        admitted_tree
    );
    let replacement = fixture.path.join("replacement-index");
    std::fs::write(&replacement, b"unchecked malformed replacement").unwrap();
    std::fs::rename(&replacement, &original).unwrap();
    let mut fixed = resolve::parse_index(&admitted).unwrap();
    assert_eq!(
        fixed.write_tree_to(&fixture.repository).unwrap(),
        admitted_tree
    );
    assert!(resolve::read_index(&original).is_err());
    assert_eq!(
        crate::bounded_file::read(&alternate, MAX_OUTPUT).unwrap(),
        admitted
    );
}

#[test]
fn absent_index_is_empty_and_conflict_stages_are_rejected() {
    let fixture = Fixture::new();
    let missing = fixture.path.join("no-index");
    let mut empty = resolve::read_index(&missing).unwrap();
    assert_eq!(empty.len(), 0);
    assert_eq!(
        empty
            .write_tree_to(&fixture.repository)
            .unwrap()
            .to_string(),
        EMPTY_TREE
    );
    assert!(!missing.exists());
    fixture.stage("a.txt", "conflicted\n");
    let mut index = fixture.repository.index().unwrap();
    let mut entry = index.get_path(Path::new("a.txt"), 0).unwrap();
    index.remove_path(Path::new("a.txt")).unwrap();
    entry.flags = (entry.flags & !0x3000) | 0x1000;
    index.add(&entry).unwrap();
    assert!(index.has_conflicts());
    index.write().unwrap();
    let error = fixture
        .source()
        .resolve(Kind::Index, "HEAD", None)
        .unwrap_err();
    assert!(error.to_string().contains("unresolved conflicts"));
}
