use super::*;
use std::os::unix::fs::symlink;

/// A source file and the run's prefix directory that mirrors it.
fn laid_out(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let root = root.canonicalize().unwrap();
    let source = root.join("pkg/a.py");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "x = 1\n").unwrap();
    let scratch = root.join("scratch");
    let parent = source.parent().unwrap().strip_prefix("/").unwrap();
    let mirrored = scratch.join("pycache").join(parent);
    fs::create_dir_all(&mirrored).unwrap();
    (source, scratch, mirrored)
}

#[test]
fn eviction_removes_every_entry_of_the_mutated_module_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (source, scratch, mirrored) = laid_out(dir.path());
    for name in [
        "a.cpython-312.pyc",
        "a.cpython-312.opt-1.pyc",
        "ab.cpython-312.pyc",
        "a.txt",
    ] {
        fs::write(mirrored.join(name), b"").unwrap();
    }
    evict_mutated_caches(&[source], &scratch).unwrap();
    let mut left: Vec<String> = fs::read_dir(&mirrored)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["a.txt", "ab.cpython-312.pyc"]);
}

#[test]
fn a_run_that_wrote_no_cache_has_nothing_to_evict() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.py");
    evict_mutated_caches(&[absent], &dir.path().join("scratch")).unwrap();
}

#[test]
fn eviction_of_a_source_that_cannot_be_resolved_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (source, scratch, _) = laid_out(dir.path());
    let error = evict_mutated_caches(&[source.with_file_name("gone.py")], &scratch).unwrap_err();
    assert!(error.starts_with("cannot resolve"), "{error}");
}

#[test]
fn eviction_refuses_to_follow_an_entry_out_of_the_runs_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let (source, scratch, mirrored) = laid_out(dir.path());
    let outside = dir.path().join("outside.pyc");
    fs::write(&outside, b"keep").unwrap();
    symlink(&outside, mirrored.join("a.cpython-312.pyc")).unwrap();
    let error = evict_mutated_caches(&[source], &scratch).unwrap_err();
    assert!(
        error.contains("not under the run's cache prefix"),
        "{error}"
    );
    assert_eq!(fs::read(&outside).unwrap(), b"keep");
}

#[test]
fn the_environment_carries_the_base_plus_a_private_writable_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = scratch_root_for(dir.path());
    let base = BTreeMap::from([
        ("KEPT".to_string(), "1".to_string()),
        ("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()),
        ("PYTHONPYCACHEPREFIX".to_string(), "/shared".to_string()),
    ]);
    let bound = isolated_python_env(&base, &scratch, &[]).unwrap();
    assert_eq!(bound["KEPT"], "1");
    assert!(!bound.contains_key("PYTHONDONTWRITEBYTECODE"));
    assert_eq!(
        bound["PYTHONPYCACHEPREFIX"],
        scratch.join("pycache").to_string_lossy()
    );
    assert!(scratch.join("pycache").is_dir());
    assert!(scratch.join(RUN_MARKER_NAME).is_file());
}

#[test]
fn binding_the_environment_evicts_the_mutated_sources() {
    let dir = tempfile::tempdir().unwrap();
    let (source, scratch, mirrored) = laid_out(dir.path());
    let cache = mirrored.join("a.cpython-312.pyc");
    fs::write(&cache, b"stale").unwrap();
    isolated_python_env(&BTreeMap::new(), &scratch, &[source]).unwrap();
    assert!(!cache.exists());
}

#[test]
fn a_scratch_that_cannot_be_used_fails_rather_than_sharing_a_cache() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, b"").unwrap();
    let error = isolated_python_env(&BTreeMap::new(), &file.join("scratch"), &[]).unwrap_err();
    assert!(error.contains("is not usable"), "{error}");
}

#[test]
fn the_scratch_root_is_one_pinned_name_under_the_childs_directory() {
    assert_eq!(
        scratch_root_for(Path::new("/w")),
        Path::new("/w/.bytecode-isolation")
    );
}
