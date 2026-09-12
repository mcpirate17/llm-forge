//! The index's contract: it answers the same questions the scans did, plus the one
//! they got wrong.

use crate::index::{build, dotted_for, walk_tests};
use std::path::{Path, PathBuf};

/// A throwaway tree. Tests that index a repository need real files on disk, because
/// walking and reading them is half of what is under test.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "slop-core-index-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tree(p)
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The tree the Python side of this package indexes: `<repo>/src`, home of the
/// `conductor` namespace package (mirrors `test_repo_index.py`'s
/// `Path(__file__).resolve().parents[1]`).
fn repo_root() -> PathBuf {
    // native/slop-core -> repository root -> src. Checked by name, not depth: when
    // the crate moved (twice now: research/runtime/native/rust -> tooling/native ->
    // native), a depth-only walk landed in /tmp and indexed 85k test files from
    // every worktree there while still clearing the "> 200 files" guard.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..2 {
        p = p.parent().unwrap().to_path_buf();
    }
    let p = p.join("src");
    assert!(
        p.join("conductor/slop_gate.py").is_file(),
        "resolved root {} is not the repository",
        p.display()
    );
    p
}

#[test]
fn from_package_import_module_binds_the_module() {
    // The defect this index exists to fix. `from conductor import slop_gate` is the
    // dominant idiom in this repository and the old matcher, which compared only the
    // `module` field of an ImportFrom against the full dotted path, never saw it.
    let t = Tree::new("frompkg");
    t.write(
        "conductor/test_gate.py",
        "from conductor import slop_gate\n",
    );
    let idx = build(t.path());
    assert_eq!(
        idx.drivers_for("conductor/slop_gate.py"),
        vec!["conductor/test_gate.py"]
    );
}

#[test]
fn the_three_import_forms_all_resolve() {
    let t = Tree::new("forms");
    t.write("pkg/test_a.py", "import pkg.target\n");
    t.write("pkg/test_b.py", "from pkg.target import thing\n");
    t.write("pkg/test_c.py", "from pkg import target\n");
    t.write("pkg/test_d.py", "import pkg.target as tgt\n");
    t.write("pkg/test_e.py", "from pkg import target as tgt\n");
    let idx = build(t.path());
    assert_eq!(
        idx.drivers_for("pkg/target.py"),
        vec![
            "pkg/test_a.py",
            "pkg/test_b.py",
            "pkg/test_c.py",
            "pkg/test_d.py",
            "pkg/test_e.py"
        ]
    );
}

#[test]
fn relative_imports_resolve_against_the_test_s_own_package() {
    let t = Tree::new("relative");
    t.write("pkg/test_here.py", "from . import target\n");
    t.write("pkg/sub/test_up.py", "from .. import target\n");
    t.write("pkg/sub/test_down.py", "from ..target import thing\n");
    let idx = build(t.path());
    assert_eq!(
        idx.drivers_for("pkg/target.py"),
        vec![
            "pkg/sub/test_down.py",
            "pkg/sub/test_up.py",
            "pkg/test_here.py"
        ]
    );
}

#[test]
fn a_relative_import_climbing_past_the_root_is_dropped_not_wrapped() {
    // `from ... import x` two levels above a top-level package has no answer. It must
    // not wrap around into some other package's namespace.
    let t = Tree::new("climb");
    t.write("pkg/test_x.py", "from ... import target\n");
    let idx = build(t.path());
    assert!(idx.drivers_for("target.py").is_empty());
    assert!(idx.drivers_for("pkg/target.py").is_empty());
}

#[test]
fn a_package_is_named_by_its_directory_not_its_init() {
    assert_eq!(dotted_for("conductor/__init__.py"), "conductor");
    assert_eq!(dotted_for("conductor/slop_gate.py"), "conductor.slop_gate");
    let t = Tree::new("init");
    t.write("pkg/test_a.py", "from pkg import thing\n");
    let idx = build(t.path());
    assert_eq!(idx.drivers_for("pkg/__init__.py"), vec!["pkg/test_a.py"]);
}

#[test]
fn named_by_matches_whole_words_only() {
    // `git grep -w -F`. A substring hit would silently reclassify a genuine coverage
    // hole as a driver-selection miss, which is the opposite of what the split is for.
    let t = Tree::new("words");
    t.write("test_a.py", "value = compute_thing()\n");
    t.write("test_b.py", "value = compute_thing_extra()\n");
    let idx = build(t.path());
    assert_eq!(idx.named_by("compute_thing"), vec!["test_a.py"]);
    assert_eq!(idx.named_by("compute_thing_extra"), vec!["test_b.py"]);
}

#[test]
fn named_by_sees_strings_and_comments_as_git_grep_does() {
    // A name reached only through getattr is exactly the indirect reference the
    // question is asking about, so a parse-based index would be wrong here.
    let t = Tree::new("strings");
    t.write("test_a.py", "fn = getattr(mod, \"hidden_helper\")\n");
    t.write("test_b.py", "# hidden_helper is covered by the sweep\n");
    let idx = build(t.path());
    assert_eq!(
        idx.named_by("hidden_helper"),
        vec!["test_a.py", "test_b.py"]
    );
}

#[test]
fn conditional_and_nested_imports_are_indexed() {
    // Optional native paths in this repository are loaded inside `try` and inside
    // functions. A test that imports a module only on one branch still drives it.
    let t = Tree::new("nested");
    t.write(
        "test_a.py",
        "def test_native():\n    from pkg import target\n    assert target\n",
    );
    t.write(
        "test_b.py",
        "try:\n    import pkg.target\nexcept ImportError:\n    pass\n",
    );
    let idx = build(t.path());
    assert_eq!(
        idx.drivers_for("pkg/target.py"),
        vec!["test_a.py", "test_b.py"]
    );
}

#[test]
fn only_test_files_are_indexed_and_vendor_trees_are_skipped() {
    let t = Tree::new("skip");
    t.write("test_real.py", "from pkg import target\n");
    t.write("helper.py", "from pkg import target\n");
    t.write(".venv/lib/test_vendored.py", "from pkg import target\n");
    t.write("node_modules/test_dep.py", "from pkg import target\n");
    let idx = build(t.path());
    assert_eq!(idx.drivers_for("pkg/target.py"), vec!["test_real.py"]);
    assert_eq!(idx.file_count(), 1);
}

#[test]
fn an_unparseable_test_still_contributes_its_names() {
    // A syntax error costs its imports, not its whole row: the word index is a byte
    // scan and cannot fail. The old code dropped the file entirely on SyntaxError.
    let t = Tree::new("broken");
    t.write("test_broken.py", "def (((:\n  mentions_a_name\n");
    let idx = build(t.path());
    assert_eq!(idx.named_by("mentions_a_name"), vec!["test_broken.py"]);
}

#[test]
fn the_index_over_this_repository_resolves_it_at_scale() {
    // Guards the resolved root as much as the index: a tool that silently walks the
    // wrong tree reports a clean, meaningless answer.
    let root = repo_root();
    let tests = walk_tests(&root);
    // The floor is derived from the tree actually under test (an independent walk),
    // not a constant pinned to one particular checkout's size -- a smaller
    // standalone tree is not "wrong", an empty or misresolved root is.
    assert!(
        !tests.is_empty(),
        "resolved root {} yielded no test files -- wrong tree",
        root.display()
    );
    let idx = build(&root);
    assert_eq!(idx.file_count(), tests.len());
    assert!(
        idx.import_key_count() > tests.len(),
        "{} import keys over {} test files",
        idx.import_key_count(),
        tests.len()
    );

    // The module whose own driver test the old matcher could not see.
    assert!(
        idx.drivers_for("conductor/slop_gate.py")
            .contains(&"conductor/test_slop_gate.py".to_string()),
        "the gate's own driver test is still unresolved"
    );
}

// The superset property -- that binding the imported names can add a driver and
// never remove one -- is checked in conductor/test_native_ablations.py, against the
// genuine `ast` matcher. A reference implementation written here would be a line
// scan, and a line scan cannot tell an import from an import quoted inside a test
// fixture; the first attempt at this test failed on exactly that and was wrong where
// the index was right.

#[test]
fn an_empty_directory_indexes_to_nothing_rather_than_guessing() {
    // The Python surface refuses a root that is not a directory; a real but empty
    // one is a legitimate answer, and must not be confused with the refusal.
    let t = Tree::new("empty");
    let idx = build(t.path());
    assert_eq!(idx.file_count(), 0);
    assert!(idx.drivers_for("pkg/target.py").is_empty());
    assert!(idx.named_by("anything").is_empty());
}
