#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for the public Python project-path contract.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)] // This helper is shared by three separate integration-test binaries.
mod support;

use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::fs;
use std::path::Path;
use support::{assert_error, attr_bool, attr_text, module, path, text, Case};

fn project_paths<'py>(
    py: Python<'py>,
    pp: &Bound<'py, PyModule>,
    root: &Path,
) -> Bound<'py, pyo3::types::PyAny> {
    pp.getattr("project_paths")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
}

fn call_path(pp: &Bound<'_, PyModule>, name: &str, root: &Bound<'_, pyo3::types::PyAny>) -> String {
    text(&pp.getattr(name).unwrap().call1((root,)).unwrap())
}

fn assert_refusal(
    py: Python<'_>,
    pp: &Bound<'_, PyModule>,
    name: &str,
    root: &Path,
    fragment: &str,
) {
    let error = pp
        .getattr(name)
        .unwrap()
        .call1((path(py, root),))
        .expect_err("configured path must be refused");
    assert_error(
        py,
        error,
        &pp.getattr("ProjectPathError").unwrap(),
        fragment,
    );
}

#[test]
fn literals_relative_validation_and_conductor_table() {
    let case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let defaults = pp.getattr("DEFAULTS").unwrap();
        for (constant, key, expected) in [
            (
                "DEFAULT_CANDIDATE_POLICY",
                "CANDIDATE_POLICY_KEY",
                "conductor/candidate_policy.toml",
            ),
            (
                "DEFAULT_MUTATION_REGISTRY",
                "MUTATION_REGISTRY_KEY",
                "conductor/mutation_campaigns/registry.json",
            ),
        ] {
            let value = pp.getattr(constant).unwrap();
            assert_eq!(text(&value), expected);
            let default = defaults.get_item(pp.getattr(key).unwrap()).unwrap();
            assert!(value.eq(default).unwrap());
        }
        for (raw, expected) in [
            ("campaigns/registry.json", "campaigns/registry.json"),
            ("  campaigns/registry.json  ", "campaigns/registry.json"),
            ("campaigns\\registry.json", "campaigns/registry.json"),
            ("registry.json", "registry.json"),
        ] {
            assert_eq!(
                text(
                    &pp.getattr("_relative")
                        .unwrap()
                        .call1((raw, "src"))
                        .unwrap()
                ),
                expected
            );
        }
        for raw in [
            "/abs/registry.json",
            "",
            "   ",
            "../registry.json",
            "a/../../b",
        ] {
            let error = pp
                .getattr("_relative")
                .unwrap()
                .call1((raw, "src"))
                .unwrap_err();
            assert_error(py, error, &pp.getattr("ProjectPathError").unwrap(), "must");
        }
        let error = pp
            .getattr("_relative")
            .unwrap()
            .call1((3, "src"))
            .unwrap_err();
        assert_error(
            py,
            error,
            &pp.getattr("ProjectPathError").unwrap(),
            "must be a string, got int",
        );

        let root = path(py, case.root());
        let table = pp.getattr("conductor_table").unwrap();
        assert_eq!(table.call1((&root,)).unwrap().len().unwrap(), 0);
        case.write("pyproject.toml", "[project]\nname = \"x\"\n");
        assert_eq!(table.call1((&root,)).unwrap().len().unwrap(), 0);
        case.write("pyproject.toml", "tool = \"not-a-table\"\n");
        assert_eq!(table.call1((&root,)).unwrap().len().unwrap(), 0);
        case.write("pyproject.toml", "[tool]\nconductor = \"nope\"\n");
        let error = table.call1((&root,)).unwrap_err();
        assert_error(
            py,
            error,
            &pp.getattr("ProjectPathError").unwrap(),
            "is not a table",
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\ncandidate_policy = \"policy.toml\"\n",
        );
        let declared = table.call1((&root,)).unwrap();
        assert_eq!(declared.len().unwrap(), 1);
        assert_eq!(
            text(&declared.get_item("candidate_policy").unwrap()),
            "policy.toml"
        );
    });
}

#[test]
fn policy_and_registry_precedence_and_derived_paths() {
    let mut case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let root = path(py, case.root());
        let defaults = project_paths(py, &pp, case.root());
        assert_eq!(
            attr_text(&defaults, "policy_relative"),
            "conductor/candidate_policy.toml"
        );
        assert_eq!(
            attr_text(&defaults, "registry_relative"),
            "conductor/mutation_campaigns/registry.json"
        );
        assert!(!attr_bool(&defaults, "policy_configured"));
        assert!(!attr_bool(&defaults, "registry_configured"));

        case.write("pyproject.toml", "[tool.conductor]\ncandidate_policy = \"candidate_policy.toml\"\nmutation_registry = \"campaigns/registry.json\"\n");
        let configured = project_paths(py, &pp, case.root());
        assert_eq!(
            attr_text(&configured, "policy_relative"),
            "candidate_policy.toml"
        );
        assert_eq!(
            attr_text(&configured, "registry_relative"),
            "campaigns/registry.json"
        );
        assert!(attr_bool(&configured, "policy_configured"));
        assert!(attr_bool(&configured, "registry_configured"));
        assert_eq!(
            attr_text(&configured, "root"),
            case.root().display().to_string()
        );
        assert_eq!(
            attr_text(&configured, "policy_path"),
            case.root()
                .join("candidate_policy.toml")
                .display()
                .to_string()
        );
        assert_eq!(
            attr_text(&configured, "registry_path"),
            case.root()
                .join("campaigns/registry.json")
                .display()
                .to_string()
        );
        assert_eq!(attr_text(&configured, "campaigns_relative"), "campaigns");
        assert_eq!(
            attr_text(&configured, "campaigns_root"),
            case.root().join("campaigns").display().to_string()
        );
        assert_eq!(
            call_path(&pp, "registry_relative", &root),
            "campaigns/registry.json"
        );
        assert_eq!(call_path(&pp, "campaigns_relative", &root), "campaigns");
        assert_eq!(
            call_path(&pp, "receipts_relative", &root),
            "campaigns/receipts"
        );
        assert_eq!(
            call_path(&pp, "registry_path", &root),
            case.root()
                .join("campaigns/registry.json")
                .display()
                .to_string()
        );
        assert_eq!(
            call_path(&pp, "campaigns_root", &root),
            case.root().join("campaigns").display().to_string()
        );
        let string_root = pp
            .getattr("project_paths")
            .unwrap()
            .call1((case.root().to_str().unwrap(),))
            .unwrap();
        assert_eq!(
            attr_text(&string_root, "root"),
            case.root().display().to_string()
        );

        case.set_env("CONDUCTOR_CANDIDATE_POLICY", "from_env.toml");
        case.set_env("CONDUCTOR_MUTATION_REGISTRY", "from_env/registry.json");
        let overridden = project_paths(py, &pp, case.root());
        assert_eq!(attr_text(&overridden, "policy_relative"), "from_env.toml");
        assert_eq!(
            attr_text(&overridden, "registry_relative"),
            "from_env/registry.json"
        );
        case.set_env("CONDUCTOR_CANDIDATE_POLICY", "   ");
        assert_eq!(
            attr_text(&project_paths(py, &pp, case.root()), "policy_relative"),
            "candidate_policy.toml"
        );
        case.set_env("CONDUCTOR_MUTATION_REGISTRY", "/etc/registry.json");
        assert_refusal(py, &pp, "project_paths", case.root(), "repo-root-relative");
        case.remove_env("CONDUCTOR_MUTATION_REGISTRY");
        case.remove_env("CONDUCTOR_CANDIDATE_POLICY");

        case.write(
            "pyproject.toml",
            "[tool.conductor]\nmutation_registry = \"campaigns/registry.json\"\n",
        );
        let independently = project_paths(py, &pp, case.root());
        assert_eq!(
            attr_text(&independently, "policy_relative"),
            "conductor/candidate_policy.toml"
        );
        assert!(!attr_bool(&independently, "policy_configured"));
        assert_eq!(
            attr_text(&independently, "registry_relative"),
            "campaigns/registry.json"
        );
        assert!(attr_bool(&independently, "registry_configured"));
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nmutation_registry = \"registry.json\"\n",
        );
        assert_eq!(call_path(&pp, "campaigns_relative", &root), ".");
        assert_eq!(call_path(&pp, "receipts_relative", &root), "receipts");
    });
}

struct PathFamily {
    key: &'static str,
    environment: &'static str,
    relative_field: &'static str,
    configured_field: &'static str,
    path_field: &'static str,
    relative_function: Option<&'static str>,
    path_function: Option<&'static str>,
    default: &'static str,
    configured: &'static str,
    overridden: &'static str,
}

fn assert_family(family: &PathFamily) {
    let mut case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let root = path(py, case.root());
        let defaults = project_paths(py, &pp, case.root());
        assert_eq!(
            attr_text(&defaults, family.relative_field),
            family.default,
            "{} default",
            family.key
        );
        assert!(
            !attr_bool(&defaults, family.configured_field),
            "{} default configured",
            family.key
        );
        assert_eq!(
            attr_text(&defaults, family.path_field),
            case.root().join(family.default).display().to_string()
        );
        if let Some(name) = family.relative_function {
            assert_eq!(call_path(&pp, name, &root), family.default);
        }
        if let Some(name) = family.path_function {
            assert_eq!(
                call_path(&pp, name, &root),
                case.root().join(family.default).display().to_string()
            );
        }

        case.write(
            "pyproject.toml",
            &format!(
                "[tool.conductor]\n{} = \"{}\"\n",
                family.key, family.configured
            ),
        );
        let configured = project_paths(py, &pp, case.root());
        assert_eq!(
            attr_text(&configured, family.relative_field),
            family.configured
        );
        assert!(attr_bool(&configured, family.configured_field));
        assert_eq!(
            attr_text(&configured, family.path_field),
            case.root().join(family.configured).display().to_string()
        );
        if let Some(name) = family.relative_function {
            assert_eq!(call_path(&pp, name, &root), family.configured);
        }
        if let Some(name) = family.path_function {
            assert_eq!(
                call_path(&pp, name, &root),
                case.root().join(family.configured).display().to_string()
            );
        }

        case.set_env(family.environment, family.overridden);
        assert_eq!(
            attr_text(&project_paths(py, &pp, case.root()), family.relative_field),
            family.overridden
        );
        if let Some(name) = family.relative_function {
            assert_eq!(call_path(&pp, name, &root), family.overridden);
        }
        if let Some(name) = family.path_function {
            assert_eq!(
                call_path(&pp, name, &root),
                case.root().join(family.overridden).display().to_string()
            );
        }
        case.remove_env(family.environment);
        case.write(
            "pyproject.toml",
            &format!("[tool.conductor]\n{} = \"/abs/bad\"\n", family.key),
        );
        assert_refusal(py, &pp, "project_paths", case.root(), "repo-root-relative");
    });
}

#[test]
fn independent_configured_path_families() {
    for family in [
        PathFamily {
            key: "package_root",
            environment: "CONDUCTOR_PACKAGE_ROOT",
            relative_field: "package_relative",
            configured_field: "package_configured",
            path_field: "package_path",
            relative_function: Some("package_relative"),
            path_function: Some("package_path"),
            default: "conductor",
            configured: "src/conductor",
            overridden: "lib/conductor",
        },
        PathFamily {
            key: "mutation_receipt_root",
            environment: "CONDUCTOR_MUTATION_RECEIPT_ROOT",
            relative_field: "receipt_root_relative",
            configured_field: "receipt_root_configured",
            path_field: "receipt_root_path",
            relative_function: Some("mutation_receipt_root_relative"),
            path_function: Some("mutation_receipt_root"),
            default: "research/reports/mutation_testing",
            configured: "campaigns/receipts",
            overridden: "scratch/mutation",
        },
        PathFamily {
            key: "notes_root",
            environment: "CONDUCTOR_NOTES_ROOT",
            relative_field: "notes_relative",
            configured_field: "notes_configured",
            path_field: "notes_path",
            relative_function: Some("notes_relative"),
            path_function: Some("notes_root"),
            default: "research/notes",
            configured: "docs",
            overridden: "knowledge/cards",
        },
        PathFamily {
            key: "notes_db",
            environment: "CONDUCTOR_NOTES_DB",
            relative_field: "notes_db_relative",
            configured_field: "notes_db_configured",
            path_field: "notes_db_path",
            relative_function: None,
            path_function: Some("notes_db_path"),
            default: "research/notes.db",
            configured: "research/runs.db",
            overridden: "var/prose.db",
        },
        PathFamily {
            key: "guardrail_allowlist",
            environment: "CONDUCTOR_GUARDRAIL_ALLOWLIST",
            relative_field: "guardrail_allowlist_relative",
            configured_field: "guardrail_allowlist_configured",
            path_field: "guardrail_allowlist_path",
            relative_function: Some("guardrail_allowlist_relative"),
            path_function: Some("guardrail_allowlist_path"),
            default: "conductor/guardrail_allowlist.json",
            configured: "policy/allow.json",
            overridden: "other/allow.json",
        },
        PathFamily {
            key: "crate_roster",
            environment: "CONDUCTOR_CRATE_ROSTER",
            relative_field: "crate_roster_relative",
            configured_field: "crate_roster_configured",
            path_field: "crate_roster_path",
            relative_function: Some("crate_roster_relative"),
            path_function: Some("crate_roster_path"),
            default: "tooling/native/crates.toml",
            configured: "native/roster.toml",
            overridden: "other/roster.toml",
        },
        PathFamily {
            key: "native_root",
            environment: "CONDUCTOR_NATIVE_ROOT",
            relative_field: "native_root_relative",
            configured_field: "native_root_configured",
            path_field: "native_root_path",
            relative_function: None,
            path_function: Some("native_root"),
            default: "tooling/native",
            configured: "native",
            overridden: "crates",
        },
        PathFamily {
            key: "memory_sources",
            environment: "CONDUCTOR_MEMORY_SOURCES",
            relative_field: "memory_sources_relative",
            configured_field: "memory_sources_configured",
            path_field: "memory_sources_path",
            relative_function: Some("memory_sources_relative"),
            path_function: None,
            default: "conductor/memory_sources.toml",
            configured: "cfg/catalog.toml",
            overridden: "env/catalog.toml",
        },
    ] {
        assert_family(&family);
    }
}

#[test]
fn discovery_respects_nearest_repository_and_host_override() {
    let mut case = Case::new();
    let outer = case.mkdir("outer");
    let inner = case.mkdir("outer/inner");
    let deep = case.mkdir("outer/inner/a/b");
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let enclosing = pp.getattr("enclosing_repo").unwrap();
        assert!(enclosing.call1((path(py, &deep),)).unwrap().is_none());
        case.mkdir("outer/.git");
        assert_eq!(
            text(&enclosing.call1((path(py, &deep),)).unwrap()),
            outer.display().to_string()
        );
        case.mkdir("outer/inner/.git");
        assert_eq!(
            text(&enclosing.call1((path(py, &deep),)).unwrap()),
            inner.display().to_string()
        );
        assert_eq!(
            text(&enclosing.call1((path(py, &inner),)).unwrap()),
            inner.display().to_string()
        );
        fs::remove_dir(inner.join(".git")).unwrap();
        fs::write(inner.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            text(&enclosing.call1((path(py, &deep),)).unwrap()),
            inner.display().to_string()
        );

        let plain = case.mkdir("plain");
        let host_root = pp.getattr("host_root").unwrap();
        assert_eq!(
            text(&host_root.call1((path(py, &plain),)).unwrap()),
            plain.display().to_string()
        );
        assert_eq!(
            text(&host_root.call1((path(py, &deep),)).unwrap()),
            inner.display().to_string()
        );
        {
            let _cwd = case.chdir("cwd");
            assert_eq!(
                text(&host_root.call0().unwrap()),
                case.root().join("cwd").display().to_string()
            );
            assert_eq!(
                text(&host_root.call1((path(py, &inner),)).unwrap()),
                inner.display().to_string()
            );
        }
        case.set_env("CONDUCTOR_HOST_ROOT", plain.to_str().unwrap());
        assert_eq!(
            text(&host_root.call1((path(py, &deep),)).unwrap()),
            plain.display().to_string()
        );
        case.set_env("CONDUCTOR_HOST_ROOT", "relative/path");
        let error = host_root.call0().unwrap_err();
        assert_error(
            py,
            error,
            &pp.getattr("ProjectPathError").unwrap(),
            "relative/path",
        );
        case.set_env(
            "CONDUCTOR_HOST_ROOT",
            case.root().join("missing").to_str().unwrap(),
        );
        let error = host_root.call0().unwrap_err();
        assert_error(
            py,
            error,
            &pp.getattr("ProjectPathError").unwrap(),
            "missing",
        );
        case.remove_env("CONDUCTOR_HOST_ROOT");
        let other = case.mkdir("other-repo");
        case.mkdir("other-repo/.git");
        {
            let _cwd = case.chdir("outer");
            assert_eq!(
                text(&host_root.call1((path(py, &other),)).unwrap()),
                other.display().to_string()
            );
        }
    });
}

#[test]
fn package_tree_root_handles_flat_src_and_installed_layouts() {
    let case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let tree_root = pp.getattr("package_tree_root").unwrap();

        let flat = case.mkdir("flat/conductor");
        case.mkdir("flat/.git");
        assert_eq!(
            text(&tree_root.call1((path(py, &flat),)).unwrap()),
            case.root().join("flat").display().to_string()
        );

        let package = case.mkdir("src_repo/src/conductor");
        case.mkdir("src_repo/.git");
        case.write(
            "src_repo/pyproject.toml",
            "[tool.conductor]\npackage_root = \"src/conductor\"\n",
        );
        assert_eq!(
            text(&tree_root.call1((path(py, &package),)).unwrap()),
            case.root().join("src_repo").display().to_string()
        );
        let nearer = case.root().join("src_repo/src");
        assert_eq!(
            call_path(&pp, "package_path", &path(py, &nearer)),
            package.display().to_string()
        );

        let installed = case.mkdir("site-packages/conductor");
        assert_eq!(
            text(&tree_root.call1((path(py, &installed),)).unwrap()),
            case.root().join("site-packages").display().to_string()
        );

        let disowned = case.mkdir("disowned/src/conductor");
        case.mkdir("disowned/.git");
        case.write(
            "disowned/pyproject.toml",
            "[tool.conductor]\npackage_root = \"elsewhere/conductor\"\n",
        );
        assert_eq!(
            text(&tree_root.call1((path(py, &disowned),)).unwrap()),
            case.root().join("disowned/src").display().to_string()
        );
        let unnamed = case.mkdir("unnamed/src/not_conductor");
        case.mkdir("unnamed/.git");
        let error = tree_root.call1((path(py, &unnamed),)).unwrap_err();
        assert_error(
            py,
            error,
            &pp.getattr("ProjectPathError").unwrap(),
            "not_conductor",
        );
    });
}

#[test]
fn package_and_native_root_match_this_checkout() {
    let case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let package = root.join("src/conductor");
        assert_eq!(
            call_path(&pp, "package_tree_root", &path(py, &package)),
            root.display().to_string()
        );
        assert_eq!(
            call_path(&pp, "package_path", &path(py, &root)),
            package.display().to_string()
        );
        assert!(attr_bool(
            &project_paths(py, &pp, &root),
            "native_root_configured"
        ));
        assert_eq!(
            call_path(&pp, "native_root", &path(py, &root)),
            root.join("native").display().to_string()
        );
        let package_resources = module(py, "conductor.package_resources");
        assert_eq!(
            text(&package_resources.getattr("_DISTRIBUTION_NAME").unwrap()),
            text(&pp.getattr("DISTRIBUTION_NAME").unwrap())
        );
        let _ = case;
    });
}

#[test]
fn distinct_roots_and_literal_defaults_stay_pinned() {
    let case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let defaults = pp.getattr("DEFAULTS").unwrap();
        for (constant, key, expected) in [
            ("DEFAULT_PACKAGE_ROOT", "PACKAGE_ROOT_KEY", "conductor"),
            (
                "DEFAULT_MUTATION_RECEIPT_ROOT",
                "MUTATION_RECEIPT_ROOT_KEY",
                "research/reports/mutation_testing",
            ),
            ("DEFAULT_NOTES_ROOT", "NOTES_ROOT_KEY", "research/notes"),
            ("DEFAULT_NOTES_DB", "NOTES_DB_KEY", "research/notes.db"),
            (
                "DEFAULT_GUARDRAIL_ALLOWLIST",
                "GUARDRAIL_ALLOWLIST_KEY",
                "conductor/guardrail_allowlist.json",
            ),
            (
                "DEFAULT_CRATE_ROSTER",
                "CRATE_ROSTER_KEY",
                "tooling/native/crates.toml",
            ),
            ("DEFAULT_NATIVE_ROOT", "NATIVE_ROOT_KEY", "tooling/native"),
        ] {
            let value = pp.getattr(constant).unwrap();
            assert_eq!(text(&value), expected);
            assert!(value
                .eq(defaults.get_item(pp.getattr(key).unwrap()).unwrap())
                .unwrap());
        }
        assert!(!pp
            .getattr("DEFAULT_NOTES_DB")
            .unwrap()
            .eq(pp.getattr("DEFAULT_NOTES_ROOT").unwrap())
            .unwrap());
        assert!(!pp
            .getattr("NATIVE_ROOT_KEY")
            .unwrap()
            .eq(pp.getattr("CRATE_ROSTER_KEY").unwrap())
            .unwrap());

        let root = path(py, case.root());
        case.write("pyproject.toml", "[tool.conductor]\nmutation_registry = \"campaigns/registry.json\"\nmutation_receipt_root = \"campaigns/receipts\"\n");
        assert_eq!(
            call_path(&pp, "receipts_relative", &root),
            "campaigns/receipts"
        );
        assert_eq!(
            call_path(&pp, "mutation_receipt_root_relative", &root),
            "campaigns/receipts"
        );
        case.write("pyproject.toml", "[tool.conductor]\nmutation_registry = \"campaigns/registry.json\"\nmutation_receipt_root = \"scratch/staging\"\n");
        assert_eq!(
            call_path(&pp, "receipts_relative", &root),
            "campaigns/receipts"
        );
        assert_eq!(
            call_path(&pp, "mutation_receipt_root_relative", &root),
            "scratch/staging"
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nnotes_root = \"research/notes\"\n",
        );
        assert!(attr_bool(
            &project_paths(py, &pp, case.root()),
            "notes_configured"
        ));
        assert_eq!(
            call_path(&pp, "notes_root", &root),
            case.root().join("research/notes").display().to_string()
        );
    });
}

#[test]
fn worktree_patterns_accept_configured_regexes_and_refuse_invalid_shapes() {
    let mut case = Case::new();
    Python::attach(|py| {
        let pp = module(py, "conductor.project_paths");
        let root = path(py, case.root());
        let patterns = pp.getattr("worktree_patterns").unwrap();
        let defaults: Vec<String> = patterns.call1((&root,)).unwrap().extract().unwrap();
        assert_eq!(
            defaults,
            [r"/tmp/llm-[\w.-]+", r"/home/\w+/Projects/LLM[\w.-]*"]
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nworktree_patterns = [\"/srv/work/[\\\\w.-]+\"]\n",
        );
        let configured: Vec<String> = patterns.call1((&root,)).unwrap().extract().unwrap();
        assert_eq!(configured, [r"/srv/work/[\w.-]+"]);
        case.set_env("CONDUCTOR_WORKTREE_PATTERNS", "/should/not/apply");
        let still_configured: Vec<String> = patterns.call1((&root,)).unwrap().extract().unwrap();
        assert_eq!(still_configured, configured);
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nworktree_patterns = \"/tmp/foo\"\n",
        );
        assert_refusal(
            py,
            &pp,
            "worktree_patterns",
            case.root(),
            "must be a list of strings",
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nworktree_patterns = [\"/tmp/[unclosed\"]\n",
        );
        assert_refusal(
            py,
            &pp,
            "worktree_patterns",
            case.root(),
            "not a valid regex",
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nworktree_patterns = [\"\"]\n",
        );
        assert_refusal(
            py,
            &pp,
            "worktree_patterns",
            case.root(),
            "must not be empty",
        );
    });
}
