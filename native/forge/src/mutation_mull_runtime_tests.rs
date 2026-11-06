use super::*;
use std::os::unix::fs::PermissionsExt;

fn state(root: &Path) -> State {
    let path = root.join("definition");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let mut files = Vec::new();
    for name in ["CMakeLists.txt", "mull.yml"] {
        let file = path.join(name);
        fs::write(&file, b"fixed fixture\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        files.push((
            file.clone(),
            crate::candidate_inputs::repository_live(&file, 100)
                .unwrap()
                .0,
        ));
    }
    let directory = DirectoryBinding::open(&path, None).unwrap();
    State {
        definition: RefCell::new(Some(Definition {
            path,
            directory,
            files,
        })),
        root: root.join("scratch"),
        executable: RefCell::new(None),
    }
}

#[test]
fn literal_cmake_paths_refuse_expansion_and_command_syntax() {
    assert_eq!(
        literal(Path::new("/safe/path-with_1.cpp")).unwrap(),
        "/safe/path-with_1.cpp"
    );
    for path in [
        "relative.cpp",
        "/tmp/a;b",
        "/tmp/${VAR}",
        "/tmp/\"x\"",
        "/tmp/a b",
        "/tmp/a\nb",
        "/tmp/é",
    ] {
        assert!(literal(Path::new(path)).is_err(), "{path}");
    }
}

#[test]
fn prelaunch_failure_removes_only_verified_definition_and_preserves_other_paths() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let sentinel = root.path().join("unrelated");
    fs::write(&sentinel, b"keep").unwrap();
    let error = state.prelaunch_error(anyhow::anyhow!("sandbox admission refused"));
    assert_eq!(error.to_string(), "sandbox admission refused");
    assert!(!root.path().join("definition").exists());
    assert_eq!(fs::read(sentinel).unwrap(), b"keep");
}

#[test]
fn unexpected_definition_input_is_retained_and_named_in_error() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    let path = root.path().join("definition");
    fs::write(path.join("unexpected"), b"do not silently erase").unwrap();
    let error = state.prelaunch_error(anyhow::anyhow!("sandbox admission refused"));
    assert!(format!("{error:#}").contains(path.to_str().unwrap()));
    assert!(path.join("unexpected").is_file());
    assert!(path.join("CMakeLists.txt").is_file());
}
