#[allow(dead_code)]
mod support;
use forge::memory_catalog::Catalog;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn host() -> support::Host {
    let host = support::Host::new();
    fs::write(host.0.join("pyproject.toml"), "").unwrap();
    host
}
fn load(host: &support::Host, entries: &str) -> anyhow::Result<Catalog> {
    let path = host.0.join("catalog.toml");
    fs::write(&path, format!("schema_version=1\n{entries}")).unwrap();
    Catalog::load(&host.0, Some(&path))
}
fn file(host: &support::Host, relative: &str) {
    let path = host.0.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "# Private fixture\nNo external source is read.\n").unwrap();
}
fn source(id: &str, roots: &str) -> String {
    format!("[[source]]\nid='{id}'\nkind='index'\n{roots}\n")
}

#[test]
fn host_catalog_selection_and_packaged_fallback_preserve_explicit_failure() {
    let host = host();
    let fallback = Catalog::load(&host.0, None).unwrap();
    assert_eq!(
        fallback
            .entries
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["cards", "notes", "repo-tasks"]
    );
    assert_eq!(
        fallback.scan(&BTreeSet::new()).unwrap().missing_roots.len(),
        3
    );
    assert!(Catalog::load(&host.0, Some(Path::new("absent.toml"))).is_err());
    fs::write(
        host.0.join("forge.toml"),
        "schema_version=1\n[conductor]\nmemory_sources='custom.toml'\nnotes_root='journal'\n",
    )
    .unwrap();
    assert!(Catalog::load(&host.0, None).is_err());
    file(&host, "journal/note.md");
    fs::write(
        host.0.join("custom.toml"),
        format!(
            "schema_version=1\n{}",
            source("mapped", "root='research/notes'")
        ),
    )
    .unwrap();
    let catalog = Catalog::load(&host.0, None).unwrap();
    let inventory = catalog.scan(&BTreeSet::new()).unwrap();
    assert_eq!(inventory.files.len(), 1);
    assert_eq!(
        inventory.files[0].path,
        host.0.join("journal/note.md").canonicalize().unwrap()
    );
}

#[test]
fn filename_patterns_exclusions_overlapping_roots_and_selection_are_deterministic() {
    let host = host();
    for name in [
        "notes/note1.md",
        "notes/nested/note2.md",
        "notes/private.md",
        "notes/skip/note3.md",
        "notes/other.txt",
    ] {
        file(&host, name);
    }
    let entries = format!("{}globs=['note[0-9].md','*private*']\nexclude_globs=['*private*']\nexclude_dir_names=['skip']\n{}chunk='whole'\n",
        source("notes", "roots=['notes','notes/nested','notes']"), source("other", "root='missing'"));
    let catalog = load(&host, &entries).unwrap();
    let inventory = catalog.scan(&BTreeSet::new()).unwrap();
    assert_eq!(
        inventory
            .files
            .iter()
            .map(|f| f.relative.to_string_lossy())
            .collect::<Vec<_>>(),
        ["nested/note2.md", "note1.md"]
    );
    assert_eq!(
        inventory.missing_roots,
        [("other".into(), host.0.join("missing"))]
    );
    for input in &inventory.files {
        assert_eq!(input.path, input.root.join(&input.relative));
        assert_eq!(input.mode, "heading");
        assert_eq!(input.recipe_sha256.len(), 64);
    }
    let selected = BTreeSet::from(["notes".to_owned()]);
    assert!(catalog.scan(&selected).unwrap().missing_roots.is_empty());
    assert!(catalog.scan(&BTreeSet::from(["unknown".into()])).is_err());
}

#[test]
fn absolute_file_and_include_directory_capabilities_are_supported() {
    let host = host();
    file(&host, "single.md");
    file(&host, "vault/selected/a.md");
    file(&host, "vault/ignored/b.md");
    let single = serde_json::to_string(&host.0.join("single.md")).unwrap();
    let vault = serde_json::to_string(&host.0.join("vault")).unwrap();
    let catalog = load(
        &host,
        &format!(
            "{}chunk='whole'\n{}include_dirs=['selected']\n",
            source("single", &format!("absolute_root={single}")),
            source("vault", &format!("absolute_root={vault}"))
        ),
    )
    .unwrap();
    let inventory = catalog.scan(&BTreeSet::new()).unwrap();
    assert_eq!(inventory.files.len(), 2);
    assert_eq!(inventory.files[0].source, "single");
    assert_eq!(inventory.files[0].relative, Path::new("single.md"));
    assert_eq!(inventory.files[0].mode, "whole");
    assert_eq!(inventory.files[1].relative, Path::new("a.md"));
    // Expansion is checked without scanning the inherited HOME directory.
    let catalog = load(&host, &source("home", "absolute_root='~'")).unwrap();
    assert_eq!(
        catalog.entries[0].roots,
        [std::path::PathBuf::from(std::env::var_os("HOME").unwrap())]
    );
}

#[test]
fn typed_catalog_rejects_ambiguity_traversal_duplicates_and_invalid_modes() {
    let host = host();
    for roots in [
        "root='../escape'",
        "root='/absolute'",
        "root='notes'\nroots=['notes']",
        "roots=[]",
        "include_dirs=['../escape']",
        "absolute_root='relative'",
        "root='notes'\nchunk='unknown'",
        "root='notes'\nglob='dir/*.md'",
        "root='notes'\nexclude_dir_names=['nested/dir']",
        "root='notes'\nglobs=[1]",
    ] {
        assert!(load(&host, &source("bad", roots)).is_err(), "{roots}");
    }
    let duplicate = format!(
        "{}{}",
        source("same", "root='notes'"),
        source("same", "root='other'")
    );
    assert!(load(&host, &duplicate).is_err());
    let ignored =
        "[[source]]\nid='context-only'\nkind='context'\ndescription='not an index source'\n";
    assert!(load(&host, ignored).unwrap().entries.is_empty());
    let duplicate = format!("{ignored}{}", source("context-only", "root='notes'"));
    assert!(load(&host, &duplicate).is_err());
}

#[test]
fn scanning_never_follows_descendant_or_root_ancestor_symlinks() {
    let host = host();
    file(&host, "notes/real.md");
    file(&host, "outside/private.md");
    std::os::unix::fs::symlink(host.0.join("outside"), host.0.join("notes/linked")).unwrap();
    std::os::unix::fs::symlink(
        host.0.join("outside/private.md"),
        host.0.join("notes/link.md"),
    )
    .unwrap();
    let catalog = load(&host, &source("notes", "root='notes'")).unwrap();
    assert_eq!(catalog.scan(&BTreeSet::new()).unwrap().files.len(), 1);
    let catalog = load(&host, &source("notes", "root='notes/linked'")).unwrap();
    assert!(catalog.scan(&BTreeSet::new()).is_err());
    let catalog = load(&host, &source("notes", "root='notes/linked/private.md'")).unwrap();
    assert!(catalog.scan(&BTreeSet::new()).is_err());
    let name =
        std::ffi::CString::new(host.0.join("notes/fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let catalog = load(&host, &source("notes", "root='notes'")).unwrap();
    assert!(catalog.scan(&BTreeSet::new()).is_err());
}

#[test]
fn catalog_file_limits_no_follow_and_recipe_binding_are_explicit() {
    let host = host();
    let first = load(&host, &source("notes", "root='notes'")).unwrap();
    let repeated = load(&host, &source("notes", "root='notes'")).unwrap();
    assert_eq!(
        first.entries[0].recipe_sha256,
        repeated.entries[0].recipe_sha256
    );
    let changed = load(
        &host,
        &(source("notes", "root='notes'") + "chunk='whole'\n"),
    )
    .unwrap();
    assert_ne!(
        first.entries[0].recipe_sha256,
        changed.entries[0].recipe_sha256
    );
    fs::write(host.0.join("oversized.toml"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(Catalog::load(&host.0, Some(Path::new("oversized.toml"))).is_err());
    std::os::unix::fs::symlink(host.0.join("catalog.toml"), host.0.join("linked.toml")).unwrap();
    assert!(Catalog::load(&host.0, Some(Path::new("linked.toml"))).is_err());
    let entries = (0..257)
        .map(|i| source(&format!("s{i}"), "root='notes'"))
        .collect::<String>();
    assert!(load(&host, &entries).is_err());
}

#[test]
fn repeated_root_inventory_has_linear_measurable_directory_work() {
    let host = host();
    for index in 0..512 {
        file(&host, &format!("notes/{index:04}.md"));
    }
    let catalog = load(&host, &source("notes", "roots=['notes','notes','notes']")).unwrap();
    let inventory = catalog.scan(&BTreeSet::new()).unwrap();
    assert_eq!(inventory.files.len(), 512);
    assert_eq!(inventory.examined_entries, 513);
    assert!(inventory
        .files
        .windows(2)
        .all(|pair| pair[0].path < pair[1].path));
}
