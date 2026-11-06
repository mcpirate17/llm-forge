use super::*;
use std::os::unix::fs::symlink;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn captured_config_selects_notes_and_excludes_configured_campaign_references() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "forge.toml", "schema_version=1\n[conductor]\nnotes_root='knowledge'\nmutation_registry='conductor/custom/registry.json'\n");
    write(root.path(), "pyproject.toml", "ignored legacy text");
    write(root.path(), "Makefile", "entry.py");
    write(root.path(), "conductor/custom/campaign.json", "unused.py");
    write(root.path(), "conductor/default.json", "retained.py");
    write(root.path(), ".claude/hooks/run.sh", "hook.py");
    write(root.path(), "knowledge/note.md", "noted.py");
    write(
        root.path(),
        "knowledge/__pycache__/ignored.md",
        "ignored.py",
    );
    let mut capture = Capture::open(root.path()).unwrap();
    let refs = References::capture(
        &mut capture,
        &[
            "Makefile".into(),
            "conductor/custom/campaign.json".into(),
            "conductor/default.json".into(),
        ],
    )
    .unwrap();
    assert_eq!(refs.config, "hook.py\nentry.py\nretained.py");
    assert_eq!(refs.notes, "noted.py");
    assert!(refs.includes("knowledge/new.md"));
    assert!(!refs.includes("knowledge-other/new.md"));
    assert!(!refs.includes("knowledge/new.json"));
    refs.verify(root.path()).unwrap();
    capture.verify().unwrap();
    write(root.path(), "knowledge/new.md", "new evidence");
    assert!(refs.verify(root.path()).is_err());
}

#[test]
fn reference_files_are_lossy_utf8_but_configuration_is_strict_and_join_is_exact() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "Makefile", "");
    fs::write(root.path().join("README.md"), b"a\xff").unwrap();
    let mut capture = Capture::open(root.path()).unwrap();
    let refs = References::capture(&mut capture, &["Makefile".into(), "README.md".into()]).unwrap();
    assert_eq!(refs.config, "\na\u{fffd}");
    assert_eq!(refs.notes, "");
    fs::write(root.path().join("forge.toml"), b"\xff").unwrap();
    assert!(capture.verify().is_err());
    assert!(References::capture(&mut Capture::open(root.path()).unwrap(), &[]).is_err());
}

#[test]
fn missing_reference_roots_and_selected_files_cannot_hide_symlinks() {
    for path in [".claude", "research", "forge.toml"] {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(path)).unwrap();
        let result = References::capture(&mut Capture::open(root.path()).unwrap(), &[]);
        assert!(result.is_err(), "{path}");
    }
    let root = tempfile::tempdir().unwrap();
    write(root.path(), ".claude/keep.txt", "keep");
    symlink("keep.txt", root.path().join(".claude/link.md")).unwrap();
    assert!(References::capture(&mut Capture::open(root.path()).unwrap(), &[]).is_err());
}

#[test]
fn root_replacement_disappearance_and_source_drift_are_refused() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), ".claude/keep.txt", "keep");
    let mut capture = Capture::open(root.path()).unwrap();
    let refs = References::capture(&mut capture, &[]).unwrap();
    fs::rename(root.path().join(".claude"), root.path().join("saved")).unwrap();
    write(root.path(), ".claude/keep.txt", "keep");
    assert!(refs.verify(root.path()).is_err());
    assert!(capture.verify().is_err());
    fs::remove_dir_all(root.path().join(".claude")).unwrap();
    assert!(refs.verify(root.path()).is_err());
}
