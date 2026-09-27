//! Native-only discovery and argv contracts for migrated Python API tests.

use conductor_native::test_contracts::{
    plan, registered_sources, registered_targets, validate_registry_inventory,
};
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const TEST_DIR: &str = "native/conductor-native/tests";
const REGISTRY_PATH: &str = "native/conductor-native/src/python_contract_targets.tsv";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("forge-contract-plan-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join(TEST_DIR)).unwrap();
        fs::write(
            root.join("native/conductor-native/Cargo.toml"),
            "[package]\nname='fixture'\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("native/conductor-native/src")).unwrap();
        fs::write(
            root.join(REGISTRY_PATH),
            include_str!("../src/python_contract_targets.tsv"),
        )
        .unwrap();
        for target in registered_targets().unwrap() {
            let relative = format!("{TEST_DIR}/{target}.rs");
            fs::copy(repo_root().join(&relative), root.join(relative)).unwrap();
        }
        for line in include_str!("../src/python_contract_targets.tsv").lines() {
            let Some((helper, _)) = line.split_once('\t') else {
                continue;
            };
            if helper.starts_with(&format!("{TEST_DIR}/python_contracts/"))
                || helper.starts_with(&format!("{TEST_DIR}/fixtures/"))
                || helper.ends_with(".json")
                || (helper.starts_with("src/conductor/testdata/") && helper.ends_with(".patch"))
            {
                fs::create_dir_all(root.join(helper).parent().unwrap()).unwrap();
                fs::copy(repo_root().join(helper), root.join(helper)).unwrap();
            }
        }
        Self { root }
    }
}

#[cfg(unix)]
#[test]
fn native_fixture_source_selects_contract_and_must_stay_local() {
    let fixture = Fixture::new();
    let relative = format!("{TEST_DIR}/fixtures/fixture_probe.rs");
    let file = fixture.root.join(&relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "fn main() {}\n").unwrap();
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut rows = fs::read_to_string(&registry).unwrap();
    rows.push_str(&format!("{relative}\tpython_contracts_memory_vectors\n"));
    fs::write(registry, rows).unwrap();
    let selected = plan(&fixture.root, std::slice::from_ref(&relative)).unwrap();
    assert_eq!(selected.targets, ["python_contracts_memory_vectors"]);
    fs::remove_file(&file).unwrap();
    assert!(plan(&fixture.root, std::slice::from_ref(&relative)).is_err());
    std::os::unix::fs::symlink(
        repo_root().join("native/conductor-native/src/lib.rs"),
        &file,
    )
    .unwrap();
    assert!(plan(&fixture.root, std::slice::from_ref(&relative))
        .unwrap_err()
        .to_string()
        .contains("resolves outside repository"));
    fs::remove_file(&file).unwrap();
    fs::write(&file, "include!(\"../outside.rs\");").unwrap();
    assert!(plan(&fixture.root, &[relative]).is_err());
}

#[test]
fn native_provider_paths_select_contracts_without_becoming_python_sources() {
    let fixture = Fixture::new();
    let relative = "native/conductor-native/src/dependency_probe.rs";
    fs::write(fixture.root.join(relative), "fn probe() {}\n").unwrap();
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut rows = fs::read_to_string(&registry).unwrap();
    rows.push_str(&format!("{relative}\tpython_contracts_memory_vectors\n"));
    fs::write(registry, rows).unwrap();
    let selected = plan(&fixture.root, &[relative.to_owned()]).unwrap();
    assert_eq!(selected.targets, ["python_contracts_memory_vectors"]);
    assert!(selected.source_paths.is_empty());
    assert_eq!(selected.commands[0].targets, selected.targets);
    let unrelated = plan(
        &fixture.root,
        &["native/conductor-native/src/unmapped.rs".to_owned()],
    )
    .unwrap();
    assert!(unrelated.targets.is_empty());
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut rows = fs::read_to_string(&registry).unwrap();
    rows.push_str(
        "native/conductor-native/src/nested/provider.rs\tpython_contracts_memory_vectors\n",
    );
    fs::write(registry, rows).unwrap();
    let error = plan(&fixture.root, &[relative.to_owned()]).unwrap_err();
    assert!(
        format!("{error:#}").contains("invalid contract path"),
        "{error:#}"
    );
}

#[cfg(unix)]
#[test]
fn corpus_dependencies_select_contract_and_are_validated_when_source_changes() {
    for relative in [
        "native/forge/tests/fixtures/corpus_probe.json",
        "src/conductor/testdata/mull/corpus_probe.json",
        "config/custom-campaigns/corpus_probe.json",
    ] {
        let fixture = Fixture::new();
        let file = fixture.root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "[]").unwrap();
        let registry = fixture.root.join(REGISTRY_PATH);
        let mut rows = fs::read_to_string(&registry).unwrap();
        rows.push_str(&format!("{relative}\tpython_contracts_memory_vectors\n"));
        fs::write(registry, rows).unwrap();
        assert_eq!(
            plan(&fixture.root, &[relative.to_owned()]).unwrap().targets,
            ["python_contracts_memory_vectors"]
        );
        let changed = ["src/conductor/memory_vectors.py".to_owned()];
        assert!(plan(&fixture.root, &changed)
            .unwrap()
            .targets
            .contains(&"python_contracts_memory_vectors".to_owned()));
        fs::remove_file(&file).unwrap();
        assert!(plan(&fixture.root, &changed).is_err());
        std::os::unix::fs::symlink(
            repo_root().join("native/forge/tests/fixtures/bash_pretooluse_corpus.json"),
            &file,
        )
        .unwrap();
        assert!(plan(&fixture.root, &changed)
            .unwrap_err()
            .to_string()
            .contains("not regular"));
    }
}

#[test]
fn slop_core_provider_paths_select_contracts_within_the_declared_crate() {
    let fixture = Fixture::new();
    let relative = "native/slop-core/src/provider.rs";
    let file = fixture.root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, "fn provider() {}\n").unwrap();
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut rows = fs::read_to_string(&registry).unwrap();
    rows.push_str(&format!("{relative}\tpython_contracts_memory_vectors\n"));
    fs::write(&registry, &rows).unwrap();
    let selected = plan(&fixture.root, &[relative.to_owned()]).unwrap();
    assert_eq!(selected.targets, ["python_contracts_memory_vectors"]);
    assert!(selected.source_paths.is_empty());
    for invalid in [
        "native/slop-core/src/nested/provider.rs",
        "native/unknown/src/provider.rs",
    ] {
        fs::write(
            &registry,
            format!("{rows}{invalid}\tpython_contracts_memory_vectors\n"),
        )
        .unwrap();
        let error = plan(&fixture.root, &[relative.to_owned()]).unwrap_err();
        assert!(
            format!("{error:#}").contains("invalid contract path"),
            "{error:#}"
        );
    }
}

#[cfg(unix)]
#[test]
fn patch_fixture_dependencies_are_bounded_and_required_for_selected_targets() {
    let fixture = Fixture::new();
    let relative = "src/conductor/testdata/probe/fixture.patch";
    let file = fixture.root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "historical fixture bytes\n").unwrap();
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut rows = fs::read_to_string(&registry).unwrap();
    rows.push_str(&format!("{relative}\tpython_contracts_memory_vectors\n"));
    fs::write(&registry, &rows).unwrap();
    let selected = plan(&fixture.root, &[relative.to_owned()]).unwrap();
    assert_eq!(selected.targets, ["python_contracts_memory_vectors"]);
    assert!(selected.source_paths.is_empty());
    let changed = ["src/conductor/memory_vectors.py".to_owned()];
    fs::remove_file(&file).unwrap();
    assert!(plan(&fixture.root, &changed).is_err());
    std::os::unix::fs::symlink(
        repo_root().join("native/conductor-native/Cargo.toml"),
        &file,
    )
    .unwrap();
    let error = plan(&fixture.root, &changed).unwrap_err();
    assert!(format!("{error:#}").contains("not regular"), "{error:#}");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(plan(&fixture.root, &changed).is_err());
    for invalid in [
        "src/conductor/outside.patch",
        "src/conductor/testdata/../outside.patch",
    ] {
        fs::write(
            &registry,
            format!("{rows}{invalid}\tpython_contracts_memory_vectors\n"),
        )
        .unwrap();
        let error = plan(&fixture.root, &changed).unwrap_err();
        assert!(
            format!("{error:#}").contains("invalid contract path"),
            "{error:#}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn registry_covers_every_present_contract_target_and_each_source_exists() {
    let root = repo_root();
    validate_registry_inventory(&root).unwrap();
    let targets = registered_targets().unwrap();
    let sources = registered_sources().unwrap();
    assert!(targets.len() >= 70);
    assert!(sources.len() >= 50);
    for source in sources {
        assert!(
            root.join(&source).is_file(),
            "missing mapped source: {source}"
        );
    }
    let on_disk = fs::read_dir(root.join(TEST_DIR))
        .unwrap()
        .map(Result::unwrap)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("python_contracts_")
                .then(|| name.strip_suffix(".rs").map(str::to_owned))
                .flatten()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        targets
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        on_disk
    );
}

#[test]
fn production_sources_select_nonconvention_and_reexport_contracts() {
    let root = repo_root();
    let memory = plan(&root, &["src/conductor/memory_vectors.py".to_owned()]).unwrap();
    assert_eq!(
        memory.targets,
        [
            "python_contracts_crg_workspace_tools",
            "python_contracts_memory_vectors"
        ]
    );
    let active = plan(&root, &["src/conductor/active_state.py".to_owned()]).unwrap();
    assert_eq!(
        active.targets,
        [
            "python_contracts_active_state",
            "python_contracts_inplace_handoff",
            "python_contracts_session_preamble"
        ]
    );
    let close = plan(&root, &["src/conductor/session_close.py".to_owned()]).unwrap();
    assert_eq!(close.targets, ["python_contracts_session_close"]);
    let graph = plan(&root, &["src/conductor/graph_context.py".to_owned()]).unwrap();
    assert!(graph
        .targets
        .contains(&"python_contracts_graph_context".to_owned()));
    assert!(graph
        .targets
        .contains(&"python_contracts_agent_a2a_graph_context".to_owned()));
    let agent = plan(&root, &["src/conductor/agent_a2a.py".to_owned()]).unwrap();
    for target in [
        "python_contracts_agent_a2a",
        "python_contracts_a2a_store",
        "python_contracts_a2a_delivery",
    ] {
        assert!(
            agent.targets.contains(&target.to_owned()),
            "missing {target}"
        );
    }
    let hook = plan(&root, &["src/tooling/hooks/agent/crg_gate.py".to_owned()]).unwrap();
    assert!(hook
        .targets
        .contains(&"python_contracts_crg_gate_session".to_owned()));
    assert!(hook
        .targets
        .contains(&"python_contracts_crg_gate_worktrees".to_owned()));
}

#[test]
fn selection_is_stable_deduplicated_and_has_explicit_safe_argv() {
    let root = repo_root();
    let changed = [
        "src/conductor/graph_context.py".to_owned(),
        "./src/conductor/../conductor/memory_vectors.py".to_owned(),
        root.join("src/conductor/graph_context.py")
            .display()
            .to_string(),
    ];
    let selected = plan(&root, &changed).unwrap();
    let reverse = plan(&root, &changed.into_iter().rev().collect::<Vec<_>>()).unwrap();
    assert_eq!(selected, reverse);
    assert_eq!(
        selected.source_paths,
        [
            "src/conductor/graph_context.py",
            "src/conductor/memory_vectors.py"
        ]
    );
    assert_eq!(selected.commands.len(), 1);
    let command = &selected.commands[0];
    assert_eq!(command.cwd, root.display().to_string());
    assert_eq!(command.targets, selected.targets);
    assert_eq!(command.test_paths, selected.test_paths);
    assert_eq!(
        command.argv[0..10],
        [
            "cargo",
            "test",
            "--jobs",
            "2",
            "--offline",
            "--locked",
            "--manifest-path",
            "native/conductor-native/Cargo.toml",
            "--features",
            "python-compat-tests",
        ]
    );
    assert_eq!(
        &command.argv[command.argv.len() - 2..],
        ["--", "--test-threads=1"]
    );
    let paired = command.argv[10..command.argv.len() - 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            assert_eq!(pair[0], "--test");
            pair[1].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(paired, selected.targets);
    assert!(selected
        .test_paths
        .iter()
        .all(|path| path.starts_with(TEST_DIR)));
}

#[test]
fn changed_rust_contract_and_shared_helper_select_their_cargo_targets() {
    let root = repo_root();
    let one = plan(
        &root,
        &[format!("{TEST_DIR}/python_contracts_memory_vectors.rs")],
    )
    .unwrap();
    assert_eq!(one.targets, ["python_contracts_memory_vectors"]);
    assert!(one.source_paths.is_empty());
    let helper = plan(
        &root,
        &[format!("{TEST_DIR}/python_contracts/agent_comm_support.rs")],
    )
    .unwrap();
    assert_eq!(
        helper.targets,
        [
            "python_contracts_agent_a2a",
            "python_contracts_baseline_merge",
            "python_contracts_bash_pretooluse_parity",
            "python_contracts_bash_quiet_legacy",
            "python_contracts_branch_policy_native",
            "python_contracts_cost_budget_audit",
            "python_contracts_cost_ledger",
            "python_contracts_crg_mcp_probe",
            "python_contracts_crg_response_shim",
            "python_contracts_crg_server",
            "python_contracts_crg_server_wiring",
            "python_contracts_crg_venv_sync_core",
            "python_contracts_crg_venv_sync_session",
            "python_contracts_crg_workspace_tools",
            "python_contracts_dispatch_doctor",
            "python_contracts_dispatch_main",
            "python_contracts_dispatch_merge",
            "python_contracts_dispatch_runner_execution",
            "python_contracts_dispatch_runner_protocol",
            "python_contracts_handoff",
            "python_contracts_harness_provisioning",
            "python_contracts_import_ablation",
            "python_contracts_ledger_calibrate",
            "python_contracts_local_clerk",
            "python_contracts_mutation_coverage",
            "python_contracts_mutation_engine_mull_args",
            "python_contracts_mutation_engine_mull_rows",
            "python_contracts_mutation_engine_mull_run",
            "python_contracts_mutation_generated_core",
            "python_contracts_mutation_generated_manifest",
            "python_contracts_mutation_generated_receipt",
            "python_contracts_mutation_receipt_encoding",
            "python_contracts_mutation_run_scope",
            "python_contracts_mutation_support_host",
            "python_contracts_mutation_support_process",
            "python_contracts_mutation_testing_core",
            "python_contracts_mutation_testing_evidence",
            "python_contracts_post_tool_parity",
            "python_contracts_post_tool_quiet",
            "python_contracts_reuse_audit_inventory",
            "python_contracts_reuse_consolidation",
            "python_contracts_reuse_file_families",
            "python_contracts_reuse_file_family_lsh",
            "python_contracts_reuse_inventory",
            "python_contracts_reuse_roi",
            "python_contracts_session_brief",
        ]
    );
    let generic = plan(&root, &[format!("{TEST_DIR}/python_contracts/support.rs")]).unwrap();
    assert!(generic.targets.len() >= 70);
    assert!(!generic
        .targets
        .contains(&"python_contracts_discovery".to_owned()));
}

#[test]
fn missing_or_unregistered_contract_target_fails_closed() {
    let fixture = Fixture::new();
    fs::remove_file(
        fixture
            .root
            .join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs")),
    )
    .unwrap();
    let error = plan(
        &fixture.root,
        &["src/conductor/memory_vectors.py".to_owned()],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("mapped contract file missing"), "{error}");
    fs::write(
        fixture
            .root
            .join(format!("{TEST_DIR}/python_contracts_unregistered.rs")),
        "// fixture\n",
    )
    .unwrap();
    fs::write(
        fixture
            .root
            .join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs")),
        fs::read(repo_root().join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs")))
            .unwrap(),
    )
    .unwrap();
    let error = plan(
        &fixture.root,
        &[format!("{TEST_DIR}/python_contracts_unregistered.rs")],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("changed contract target missing from registry"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn selected_helper_must_exist_be_registered_and_stay_inside_snapshot() {
    let fixture = Fixture::new();
    let changed = ["src/conductor/memory_vectors.py".to_owned()];
    let helper = fixture
        .root
        .join(format!("{TEST_DIR}/python_contracts/support.rs"));
    fs::remove_file(&helper).unwrap();
    assert!(plan(&fixture.root, &changed)
        .unwrap_err()
        .to_string()
        .contains("mapped contract file missing"));
    std::os::unix::fs::symlink(
        repo_root().join(format!("{TEST_DIR}/python_contracts/support.rs")),
        &helper,
    )
    .unwrap();
    assert!(plan(&fixture.root, &changed)
        .unwrap_err()
        .to_string()
        .contains("not regular"));
    fs::remove_file(&helper).unwrap();
    fs::write(&helper, "// helper\n").unwrap();
    let target = fixture
        .root
        .join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs"));
    fs::write(&target, "#[path = \"../outside.rs\"]\nmod outside;\n").unwrap();
    assert!(plan(&fixture.root, &changed)
        .unwrap_err()
        .to_string()
        .contains("unsupported contract helper path"));
    fs::write(&target, "// no longer includes registered helper\n").unwrap();
    assert!(plan(&fixture.root, &changed)
        .unwrap_err()
        .to_string()
        .contains("contract helper registry differs"));
}

#[test]
fn registry_selects_non_python_hook_dependencies_and_rejects_unsafe_rows() {
    let root = repo_root();
    for changed in [
        ".claude/hooks/dispatch.py",
        "src/tooling/hooks/claude/settings.dispatcher.json",
        "src/tooling/hooks/claude/session-start.sh",
        "src/tooling/hooks/claude/session-handoff.sh",
    ] {
        let selected = plan(&root, &[changed.to_owned()]).unwrap();
        assert!(selected
            .targets
            .contains(&"python_contracts_dispatch_registry".to_owned()));
    }
    let fixture = Fixture::new();
    for unsafe_path in [
        "src/../outside.py",
        "src/./invalid.py",
        "src//invalid.py",
        "src/bad\\path.py",
        "src/bad\tpath.py",
    ] {
        fs::write(
            fixture.root.join(REGISTRY_PATH),
            format!("{unsafe_path}\tpython_contracts_dispatch_registry\n"),
        )
        .unwrap();
        assert!(
            plan(&fixture.root, &[REGISTRY_PATH.to_owned()]).is_err(),
            "{unsafe_path}"
        );
    }
}

#[test]
fn rust_syntax_cannot_hide_an_external_helper_from_the_registry() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join(REGISTRY_PATH),
        "src/conductor/memory_vectors.py\tpython_contracts_memory_vectors\n",
    )
    .unwrap();
    let target = fixture
        .root
        .join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs"));
    for source in [
        "#[path=\"../outside.rs\"] mod hidden;",
        "#[path = r#\"../outside.rs\"#]\nmod hidden;",
        "#[path\n=\n\"../outside.rs\"] mod hidden;",
        "mod unregistered;",
        "#[cfg_attr(unix, path=\"../outside.rs\")] mod hidden;",
        "include!(\"../outside.rs\");",
        "fn fixture() { include!(\"../outside.rs\"); }",
    ] {
        fs::write(&target, source).unwrap();
        assert!(
            plan(
                &fixture.root,
                &["src/conductor/memory_vectors.py".to_owned()]
            )
            .is_err(),
            "{source}"
        );
    }
    fs::write(
        &target,
        "const FIXTURE: &str = include_str!(\"../fixture.txt\");",
    )
    .unwrap();
    assert!(plan(
        &fixture.root,
        &["src/conductor/memory_vectors.py".to_owned()]
    )
    .is_ok());
}

#[cfg(unix)]
#[test]
fn runtime_requires_both_native_manifests_inside_the_candidate_snapshot() {
    let fixture = Fixture::new();
    let forge = fixture.root.join("native/forge/Cargo.toml");
    fs::create_dir_all(forge.parent().unwrap()).unwrap();
    fs::write(&forge, "[package]\nname='forge'\n").unwrap();
    let request = serde_json::json!({
        "snapshot": fixture.root, "runtime_dir": "/tmp/contract-runtime",
        "python_executable": "/usr/bin/python3", "targets": ["python_contracts_memory_vectors"],
    });
    let runtime =
        || conductor_native::candidate_verification::decide("contract_runtime_plan", &request);
    assert!(runtime().is_ok());
    for manifest in [
        "native/forge/Cargo.toml",
        "native/conductor-native/Cargo.toml",
    ] {
        let file = fixture.root.join(manifest);
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(repo_root().join(manifest), &file).unwrap();
        assert!(runtime().unwrap_err().contains("not regular"));
        fs::remove_file(&file).unwrap();
        fs::write(&file, "[package]\nname='fixture'\n").unwrap();
    }
    fs::remove_dir_all(fixture.root.join("native/forge")).unwrap();
    std::os::unix::fs::symlink(
        repo_root().join("native/forge"),
        fixture.root.join("native/forge"),
    )
    .unwrap();
    assert!(runtime().unwrap_err().contains("escapes its crate"));
}

#[test]
fn candidate_registry_addition_is_seen_without_rebuilding_the_planner() {
    let fixture = Fixture::new();
    let added = "src/conductor/newly_migrated.py";
    assert!(!registered_sources().unwrap().contains(&added.to_owned()));
    let registry = fixture.root.join(REGISTRY_PATH);
    let mut contents = fs::read_to_string(&registry).unwrap();
    contents.push_str(&format!("{added}\tpython_contracts_memory_vectors\n"));
    fs::write(registry, contents).unwrap();
    let selected = plan(&fixture.root, &[added.to_owned()]).unwrap();
    assert_eq!(selected.source_paths, [added]);
    assert_eq!(selected.targets, ["python_contracts_memory_vectors"]);
    assert_eq!(selected.commands.len(), 1);
}

#[test]
fn registry_only_change_selects_every_registered_target() {
    let fixture = Fixture::new();
    let selected = plan(&fixture.root, &[REGISTRY_PATH.to_owned()]).unwrap();
    assert_eq!(selected.targets, registered_targets().unwrap());
    assert!(selected.source_paths.is_empty());
    assert_eq!(selected.commands.len(), 1);
    assert_eq!(selected.commands[0].targets, selected.targets);
    assert_eq!(selected.commands[0].test_paths, selected.test_paths);
}

#[test]
fn registry_change_cannot_drop_the_last_mapping_for_an_existing_target() {
    let fixture = Fixture::new();
    let registry = fixture.root.join(REGISTRY_PATH);
    let contents = fs::read_to_string(&registry).unwrap();
    let filtered = contents
        .lines()
        .filter(|line| !line.ends_with("\tpython_contracts_memory_vectors"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(registry, format!("{filtered}\n")).unwrap();
    let error = plan(&fixture.root, &[REGISTRY_PATH.to_owned()])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("contract target missing from registry"),
        "{error}"
    );
}

#[test]
fn missing_malformed_or_unregistered_registry_target_cannot_pass_as_empty() {
    let fixture = Fixture::new();
    let registry = fixture.root.join(REGISTRY_PATH);
    fs::remove_file(&registry).unwrap();
    for changed in [
        "src/conductor/memory_vectors.py",
        REGISTRY_PATH,
        "README.md",
    ] {
        let error = plan(&fixture.root, &[changed.to_owned()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("contract registry missing"), "{error}");
    }
    fs::write(&registry, "not-a-tabbed-registry-row\n").unwrap();
    let error = plan(&fixture.root, &[REGISTRY_PATH.to_owned()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid contract registry"), "{error}");
    fs::write(
        &registry,
        "src/conductor/newly_migrated.py\tpython_contracts_missing\n",
    )
    .unwrap();
    let error = plan(&fixture.root, &[REGISTRY_PATH.to_owned()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("mapped contract file missing"), "{error}");
}

#[cfg(unix)]
#[test]
fn selected_registry_must_be_a_regular_file_inside_the_repository() {
    let fixture = Fixture::new();
    let registry = fixture.root.join(REGISTRY_PATH);
    fs::remove_file(&registry).unwrap();
    std::os::unix::fs::symlink("/tmp/external-contract-registry.tsv", &registry).unwrap();
    let error = plan(&fixture.root, &[REGISTRY_PATH.to_owned()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("not regular"), "{error}");
}

#[cfg(unix)]
#[test]
fn mapped_contract_cannot_be_a_symlink_outside_its_crate() {
    let fixture = Fixture::new();
    let target = fixture
        .root
        .join(format!("{TEST_DIR}/python_contracts_memory_vectors.rs"));
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink("/tmp/outside-contract.rs", &target).unwrap();
    let error = plan(
        &fixture.root,
        &["src/conductor/memory_vectors.py".to_owned()],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("not regular"), "{error}");
}

#[cfg(unix)]
#[test]
fn absolute_changed_path_accepts_the_given_symlinked_repo_root() {
    let fixture = Fixture::new();
    let alias = fixture.root.with_extension("alias");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let changed = alias.join("src/conductor/memory_vectors.py");
    let selected = plan(&alias, &[changed.display().to_string()]).unwrap();
    fs::remove_file(alias).unwrap();
    assert_eq!(selected.source_paths, ["src/conductor/memory_vectors.py"]);
    assert_eq!(
        selected.targets,
        [
            "python_contracts_crg_workspace_tools",
            "python_contracts_memory_vectors"
        ]
    );
}

#[test]
fn path_boundaries_and_unmapped_paths_do_not_create_shell_arguments() {
    let root = repo_root();
    for escaped in [
        "../outside.py".to_owned(),
        "/tmp/outside.py".to_owned(),
        "src/conductor/memory_vectors.py\n--test=bad".to_owned(),
        "src\\conductor\\memory_vectors.py".to_owned(),
    ] {
        assert!(plan(&root, &[escaped]).is_err());
    }
    let empty = plan(&root, &["src/conductor/unknown.py; echo unsafe".to_owned()]).unwrap();
    assert!(empty.targets.is_empty());
    assert!(empty.source_paths.is_empty());
    assert!(empty.test_paths.is_empty());
    assert!(empty.commands.is_empty());
}

#[test]
fn unrelated_host_paths_need_no_forge_crate_or_contract_files() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.root.join("native")).unwrap();
    let empty = plan(
        &fixture.root,
        &[
            "conductor/memory_vectors.py".to_owned(),
            "README.md".to_owned(),
            "src/conductor/unmapped.py".to_owned(),
        ],
    )
    .unwrap();
    assert!(empty.targets.is_empty());
    assert!(empty.source_paths.is_empty());
    assert!(empty.test_paths.is_empty());
    assert!(empty.commands.is_empty());
    let error = plan(
        &fixture.root,
        &["src/conductor/memory_vectors.py".to_owned()],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("contract registry missing"), "{error}");
}

#[cfg(feature = "python")]
#[test]
fn python_binding_returns_targets_paths_and_argv_as_data() {
    let root = repo_root();
    Python::initialize();
    Python::attach(|py| {
        let module = pyo3::types::PyModule::new(py, "contracts").unwrap();
        conductor_native::test_contracts::register(&module).unwrap();
        let response: String = module
            .getattr("contract_test_plan_native")
            .unwrap()
            .call1((
                root.display().to_string(),
                vec!["src/conductor/memory_vectors.py"],
            ))
            .unwrap()
            .extract()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["targets"],
            serde_json::json!([
                "python_contracts_crg_workspace_tools",
                "python_contracts_memory_vectors"
            ])
        );
        assert_eq!(
            value["test_paths"],
            serde_json::json!([
                "native/conductor-native/tests/python_contracts_crg_workspace_tools.rs",
                "native/conductor-native/tests/python_contracts_memory_vectors.rs"
            ])
        );
        assert_eq!(value["commands"][0]["argv"][0], "cargo");
        assert_eq!(
            value["source_paths"],
            serde_json::json!(["src/conductor/memory_vectors.py"])
        );
        assert_eq!(value["commands"][0]["targets"], value["targets"]);
        assert_eq!(value["commands"][0]["test_paths"], value["test_paths"]);
    });
}
