use super::*;
use git2::{Repository, Signature, Time};
use std::fs;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("diff.renames", false)
        .unwrap();
    let files = [
        ("forge.toml", "schema_version=1\n[conductor]\nnotes_root='knowledge'\nmutation_registry='conductor/custom/registry.json'\n"),
        ("pkg/__init__.py", ""),
        ("pkg/subject.py", "import pkg.local\n"),
        ("pkg/lazy.py", "def later():\n    import pkg.retired\n"),
        ("pkg/configured.py", "VALUE = 1\n"),
        ("tests/test_broken.py", "import pkg.subject\nimport pkg.missing\n"),
        ("tests/test_subject.py", "import pkg.subject\n"),
        ("tests/test_configured.py", "import pkg.configured\n"),
        ("Makefile", "pkg.configured"),
        ("conductor/custom/campaign.json", "pkg.subject"),
        (".gitignore", "ignored/\n"),
    ];
    let mut index = repo.index().unwrap();
    for (path, content) in files {
        write(root.path(), path, content);
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::new("Fixture", "fixture@example.invalid", &Time::new(0, 0)).unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    write(root.path(), "pkg/local.py", "LOCAL = 1\n");
    write(root.path(), "knowledge/note.md", "pkg.subject");
    write(root.path(), "tests/test_new.py", "assert True\n");
    write(root.path(), "ignored/test_hidden.py", "assert True\n");
    root
}

#[test]
fn native_coordinator_preserves_classification_history_and_untracked_tests() {
    let root = fixture();
    let snapshot = Snapshot::capture(root.path()).unwrap();
    snapshot.verify().unwrap();
    let report = &snapshot.report;
    assert_eq!(report["tests_scanned"], 3);
    assert_eq!(report["modules_scanned"], 7);
    assert_eq!(
        report["broken"],
        json!([{"test":"tests/test_broken.py","missing":["pkg.missing"],"last_commit":"1970-01-01"}])
    );
    assert_eq!(
        report["depends_on_untracked"],
        json!([{"test":"tests/test_subject.py","untracked":["pkg/local.py"],"last_commit":"1970-01-01"}])
    );
    assert_eq!(
        report["orphan_target"],
        json!([{"test":"tests/test_subject.py","targets":["pkg/subject.py"],"notes_only":["pkg/subject.py"],"last_commit":"1970-01-01"}])
    );
    assert_eq!(
        report["stale_imports"],
        json!([{"module":"pkg/lazy.py","missing":["pkg.retired"],"last_commit":"1970-01-01"}])
    );
    assert_eq!(report["untracked_tests"], json!(["tests/test_new.py"]));
    assert_eq!(
        report["untracked_importers"]["pkg/local.py"],
        json!(["pkg/subject.py"])
    );
    let envelope = snapshot.envelope().unwrap();
    assert_eq!(envelope["advisory"], true);
    assert_eq!(envelope["history"]["shallow"], false);
    assert_eq!(
        envelope["inputs"]["source_rows_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        envelope,
        Snapshot::capture(root.path()).unwrap().envelope().unwrap()
    );
}

#[test]
fn coordinator_refuses_source_reference_and_negative_lookup_drift() {
    for (path, content) in [
        ("pkg/subject.py", "VALUE = 1\n"),
        ("knowledge/new.md", "new evidence"),
        ("pkg/missing.py", "VALUE = 1\n"),
        ("tests/test_newer.py", "assert True\n"),
        ("Makefile", "pkg.subject"),
    ] {
        let root = fixture();
        let snapshot = Snapshot::capture(root.path()).unwrap();
        write(root.path(), path, content);
        assert!(snapshot.verify().is_err(), "drift in {path}");
    }
}

#[test]
fn output_is_explicit_confined_and_cannot_overwrite_sources_or_git_metadata() {
    let root = fixture();
    let snapshot = Snapshot::capture(root.path()).unwrap();
    assert_eq!(
        snapshot
            .output_path(Path::new("audit/report.json"))
            .unwrap(),
        root.path().join("audit/report.json")
    );
    for path in [
        "../outside.json",
        ".git/config.json",
        "conductor/custom/campaign.json",
        ".claude/new.json",
        "pkg/missing.py",
        "tests/test_new.py",
    ] {
        assert!(snapshot.output_path(Path::new(path)).is_err(), "{path}");
    }
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    assert!(snapshot
        .output_path(Path::new("escape/report.json"))
        .is_err());
}
