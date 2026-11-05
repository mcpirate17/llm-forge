//! Pure Rust contract for host path selection and repository discovery.

use conductor_native::project_paths::{
    enclosing_repo, host_root, integration_branch, package_tree_root, relative, resolve,
    retired_integration_branches, worktree_patterns,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);

struct Case(PathBuf);

impl Case {
    fn new() -> Self {
        let id = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "conductor-project-paths-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn root(&self) -> &Path {
        &self.0
    }

    fn write(&self, content: &str) {
        fs::write(self.root().join("pyproject.toml"), content).unwrap();
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn relative_paths_follow_posix_normalization_and_reject_escape() {
    for (raw, expected) in [("a/b", "a/b"), (" a\\b ", "a/b"), ("a//./b/", "a/b")] {
        assert_eq!(relative(raw, "src").unwrap(), expected);
    }
    for raw in ["", "  ", ".", "/absolute", "../escape", "a/../escape"] {
        assert!(relative(raw, "src").is_err(), "{raw:?}");
    }
}

#[test]
fn captured_policy_config_is_not_reopened_from_a_replaced_path() {
    let case = Case::new();
    let path = case.root().join("forge.toml");
    let config = conductor_native::project_config::parse_native(
        &path,
        "schema_version=1\n[conductor]\ncandidate_policy='policy/original.toml'\n",
    )
    .unwrap();
    fs::write(&path, "invalid replacement TOML [[").unwrap();
    assert_eq!(
        conductor_native::project_paths::candidate_policy_relative(&config).unwrap(),
        ("policy/original.toml".into(), true)
    );
    assert!(resolve(case.root()).is_err());
}

#[test]
fn invalid_utf8_policy_override_is_refused_without_config_fallback() {
    const CHILD: &str = "FORGE_PROJECT_PATH_UTF8_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let config = conductor_native::project_config::parse_native(
            Path::new("absent.toml"),
            "schema_version=1\n[conductor]\ncandidate_policy='fallback.toml'\n",
        )
        .unwrap();
        let error =
            conductor_native::project_paths::candidate_policy_relative(&config).unwrap_err();
        assert_eq!(error, "CONDUCTOR_CANDIDATE_POLICY must be UTF-8");
        assert_eq!(resolve(Path::new("absent-root")).unwrap_err(), error);
        return;
    }
    use std::os::unix::ffi::OsStringExt;
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "invalid_utf8_policy_override_is_refused_without_config_fallback",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env(
            "CONDUCTOR_CANDIDATE_POLICY",
            std::ffi::OsString::from_vec(b"invalid\xff".to_vec()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn eleven_paths_resolve_independently_with_configured_flags() {
    let case = Case::new();
    let defaults = resolve(case.root()).unwrap();
    assert_eq!(defaults.len(), 11);
    assert_eq!(
        defaults[0],
        ("conductor/candidate_policy.toml".into(), false)
    );
    assert_eq!(
        defaults[1],
        ("conductor/mutation_campaigns/registry.json".into(), false)
    );
    assert_eq!(defaults[2], ("conductor".into(), false));
    assert_eq!(defaults[9], ("tooling/native".into(), false));
    assert!(defaults.iter().all(|(_, configured)| !configured));

    case.write("[tool.conductor]\nmutation_registry = 'campaigns/registry.json'\npackage_root = 'src/conductor'\n");
    let configured = resolve(case.root()).unwrap();
    assert_eq!(configured[0], defaults[0]);
    assert_eq!(configured[1], ("campaigns/registry.json".into(), true));
    assert_eq!(configured[2], ("src/conductor".into(), true));
    assert_eq!(configured[3], defaults[3]);
}

#[test]
fn invalid_host_table_and_values_fail_with_source() {
    let case = Case::new();
    case.write("[tool.conductor\n");
    let error = resolve(case.root()).unwrap_err();
    assert!(error.contains("pyproject.toml"), "{error}");
    assert!(error.contains("invalid TOML configuration"), "{error}");
    case.write("[tool]\nconductor = 'nope'\n");
    assert!(resolve(case.root()).unwrap_err().contains("is not a table"));
    case.write("[tool.conductor]\nnotes_root = 3\n");
    assert!(resolve(case.root())
        .unwrap_err()
        .contains("must be a string, got int"));
    case.write("[tool.conductor]\nnotes_root = '../outside'\n");
    assert!(resolve(case.root())
        .unwrap_err()
        .contains("must be repo-root-relative"));
}

#[test]
fn branch_and_worktree_history_resolve_from_host() {
    let case = Case::new();
    assert_eq!(integration_branch(case.root()).unwrap(), "main");
    assert!(retired_integration_branches(case.root())
        .unwrap()
        .is_empty());
    assert_eq!(worktree_patterns(case.root()).unwrap().len(), 2);

    case.write("[tool.conductor]\nintegration_branch = ' trunk '\nretired_integration_branches = [' main ', ' master ']\nworktree_patterns = [' /srv/work/.* ']\n");
    assert_eq!(integration_branch(case.root()).unwrap(), "trunk");
    assert_eq!(
        retired_integration_branches(case.root()).unwrap(),
        vec!["main".to_owned(), "master".to_owned()]
    );
    assert_eq!(
        worktree_patterns(case.root()).unwrap(),
        vec!["/srv/work/.*".to_owned()]
    );
    case.write("[tool.conductor]\nworktree_patterns = '/tmp/x'\n");
    assert!(worktree_patterns(case.root())
        .unwrap_err()
        .contains("list of strings"));
}

#[test]
fn repository_and_package_discovery_prefer_host_configuration() {
    let case = Case::new();
    let repo = case.root().join("repo");
    let package = repo.join("src/conductor");
    fs::create_dir_all(&package).unwrap();
    fs::create_dir(repo.join(".git")).unwrap();
    fs::write(
        repo.join("pyproject.toml"),
        "[tool.conductor]\npackage_root = 'src/conductor'\n",
    )
    .unwrap();
    assert_eq!(enclosing_repo(&package), Some(repo.clone()));
    assert_eq!(host_root(Some(&package)).unwrap(), repo);
    assert_eq!(package_tree_root(&package).unwrap(), repo);
}

#[test]
fn selected_path_does_not_resolve_unrelated_invalid_keys() {
    let case = Case::new();
    case.write("[tool.conductor]\nmutation_receipt_root='custom/evidence'\nnotes_root=12\n");
    let selected = conductor_native::project_paths::resolve_key;
    assert_eq!(
        selected(case.root(), "mutation_receipt_root").unwrap(),
        ("custom/evidence".into(), true)
    );
    assert!(selected(case.root(), "notes_root")
        .unwrap_err()
        .contains("must be a string"));
    assert_eq!(
        selected(case.root(), "unknown").unwrap_err(),
        "unknown project path key: unknown"
    );
}
