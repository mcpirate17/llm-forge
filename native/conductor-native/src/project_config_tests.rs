use super::*;
use crate::project_paths;
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::symlink};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-project-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, body: &str) {
        fs::write(self.0.join(name), body).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn native_manifest_is_authoritative_without_merging_legacy_values() {
    let f = Fixture::new();
    f.write(
        "pyproject.toml",
        "[tool.conductor]\nintegration_branch='legacy'\nnotes_root='old-notes'\n",
    );
    f.write("forge.toml", "schema_version=1\n[conductor]\nintegration_branch='trunk'\ncandidate_policy='config/review.toml'\nmutation_registry='checks/registry.json'\nretired_integration_branches=['retired']\nworktree_patterns=['/tmp/project-.*']\n");
    let config = load(&f.0).unwrap();
    assert_eq!(config.namespace, "conductor");
    assert!(!config.conductor.contains_key("notes_root"));
    assert_eq!(project_paths::integration_branch(&f.0).unwrap(), "trunk");
    let paths = project_paths::resolve(&f.0).unwrap();
    assert_eq!(paths[0], ("config/review.toml".into(), true));
    assert_eq!(paths[1], ("checks/registry.json".into(), true));
    assert_eq!(paths[4], ("research/notes".into(), false));
    assert_eq!(
        project_paths::retired_integration_branches(&f.0).unwrap(),
        ["retired"]
    );
    assert_eq!(
        project_paths::worktree_patterns(&f.0).unwrap(),
        ["/tmp/project-.*"]
    );
    // A malformed legacy file is not read when a native manifest exists.
    f.write("pyproject.toml", "invalid = [");
    assert_eq!(project_paths::integration_branch(&f.0).unwrap(), "trunk");
}

#[test]
fn absent_native_manifest_reads_legacy_host_data_and_absent_both_use_defaults() {
    let f = Fixture::new();
    assert!(load(&f.0).unwrap().conductor.is_empty());
    assert_eq!(project_paths::integration_branch(&f.0).unwrap(), "main");
    f.write(
        "pyproject.toml",
        "[tool.conductor]\nintegration_branch='stable'\n",
    );
    assert_eq!(load(&f.0).unwrap().namespace, "tool.conductor");
    assert_eq!(project_paths::integration_branch(&f.0).unwrap(), "stable");
    f.write("forge.toml", "schema_version=1\n");
    assert_eq!(project_paths::integration_branch(&f.0).unwrap(), "main");
}

#[test]
fn malformed_native_data_never_falls_back_to_legacy() {
    let f = Fixture::new();
    f.write(
        "pyproject.toml",
        "[tool.conductor]\nintegration_branch='legacy'\n",
    );
    for body in [
        "[conductor]",
        "schema_version=2",
        "schema_version='1'",
        "schema_version=1\nconductor=4",
        "schema_version=1\nschema_version=1",
        "schema_version=1\n[conductor]\nx=[",
    ] {
        f.write("forge.toml", body);
        assert!(load(&f.0).unwrap_err().contains("forge.toml"), "{body}");
    }
    fs::write(f.0.join("forge.toml"), [0xff]).unwrap();
    assert!(load(&f.0).unwrap_err().contains("invalid TOML"));
}

#[test]
fn invalid_paths_and_branch_values_name_the_authoritative_source() {
    let f = Fixture::new();
    for value in ["''", "17", "'../outside'", "'/absolute'"] {
        f.write(
            "forge.toml",
            &format!("schema_version=1\n[conductor]\ncandidate_policy={value}\n"),
        );
        let error = project_paths::resolve(&f.0).unwrap_err();
        assert!(error.contains("[conductor].candidate_policy"), "{error}");
        assert!(error.contains("forge.toml"), "{error}");
    }
    for value in ["''", "17"] {
        f.write(
            "forge.toml",
            &format!("schema_version=1\n[conductor]\nintegration_branch={value}\n"),
        );
        assert!(project_paths::integration_branch(&f.0)
            .unwrap_err()
            .contains("forge.toml"));
    }
}

#[test]
fn configuration_refuses_symlinks_special_files_and_oversized_inputs() {
    let f = Fixture::new();
    for name in ["forge.toml", "pyproject.toml"] {
        let path = f.0.join(name);
        symlink("absent", &path).unwrap();
        assert!(load(&f.0).is_err());
        fs::remove_file(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid pathname, no descriptor or pointer retained by mkfifo.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(load(&f.0).unwrap_err().contains("regular file"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load(&f.0).unwrap_err().contains("regular file"));
        fs::remove_dir(&path).unwrap();
        fs::write(&path, vec![b' '; CONFIG_LIMIT + 1]).unwrap();
        assert!(load(&f.0).unwrap_err().contains("64 KiB"));
        fs::remove_file(&path).unwrap();
    }
}
