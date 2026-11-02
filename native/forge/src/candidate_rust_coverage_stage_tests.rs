use super::*;
use crate::{
    candidate_command::PreparedArgv,
    candidate_stage_artifact::{ArtifactBudget, Capture},
    cgroup::Cgroup,
    confined_execution::{self, Limits, Outcome},
};
use std::sync::Arc;
fn inputs() -> (String, String, String, String, String) {
    (
        std::env::var("FORGE_TEST_CGROUP_PARENT").expect("delegated cgroup"),
        std::env::var("FORGE_TEST_BIN").expect("forge binary"),
        std::env::var("FORGE_TEST_CARGO_BIN").expect("Cargo bin"),
        std::env::var("FORGE_TEST_LLVM_COV").expect("LLVM cov"),
        std::env::var("FORGE_TEST_LLVM_PROFDATA").expect("LLVM profdata"),
    )
}
fn candidate(f: &Fixture) -> (String, Vec<u8>) {
    fs::create_dir(f.host.join("src")).unwrap();
    fs::write(
        f.host.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        f.host.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = 'fixture'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(
        f.host.join("src/lib.rs"),
        "pub fn answer() -> u8 {\n 42\n}\n#[test] fn test_answer() { assert_eq!(answer(),42); }\n",
    )
    .unwrap();
    fs::write(
        f.host.join("build.rs"),
        "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
    )
    .unwrap();
    let repository = git2::Repository::open(&f.host).unwrap();
    let mut index = repository.index().unwrap();
    for path in ["Cargo.toml", "Cargo.lock", "build.rs", "src/lib.rs"] {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let before = fs::read(f.host.join(".git/index")).unwrap();
    (index.write_tree().unwrap().to_string(), before)
}
fn grants(f: &Fixture, parent: &str, forge: &str, cargo: &str, cov: &str, prof: &str) -> Grants {
    let toolroot = Path::new(cargo).parent().unwrap();
    let mut value = f.value();
    value["cgroup_parent"] = json!(parent);
    value["resources"]["memory_mib"] = json!(2048);
    // LLVM's linker uses a host-sized thread pool in addition to Cargo's workers.
    value["resources"]["processes"] = json!(128);
    value["read_only"] = json!([
        f.host,
        "/usr",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        forge,
        toolroot,
        cov,
        prof
    ]);
    value["programs"] = json!({"forge":forge,"llvm-cov":cov,"llvm-profdata":prof});
    value["environment"] = json!({"FORGE_CARGO_BIN":cargo,"FORGE_CARGO_EXTRA_PATH":"/usr/bin"});
    f.write_config(value);
    f.grants()
}
fn argv(root: &Path, tree: String, cov: String, prof: String) -> Vec<String> {
    vec![
        "forge".into(),
        "candidate".into(),
        "cargo-test".into(),
        "--repo".into(),
        root.to_str().unwrap().into(),
        "--base".into(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        "--tree".into(),
        tree,
        "--manifest=Cargo.toml".into(),
        "--bind=src/lib.rs".into(),
        "--coverage".into(),
        "--llvm-cov".into(),
        cov,
        "--llvm-profdata".into(),
        prof,
        "--execute".into(),
    ]
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicit delegated cgroup, FORGE_TEST_BIN, FORGE_TEST_CARGO_BIN, FORGE_TEST_LLVM_COV and FORGE_TEST_LLVM_PROFDATA"]
async fn real_stage_instruments_tests_and_captures_bound_rust_lines() {
    let (parent, forge, cargo, cov, prof) = inputs();
    let f = Fixture::new();
    let (tree, index_before) = candidate(&f);
    let grants = grants(&f, &parent, &forge, &cargo, &cov, &prof);
    let source = f.source();
    let workspace = Workspace::new(&source, &tree, &grants).unwrap();
    let scratch = workspace.scratch.path.clone();
    let root = workspace.path();
    let plan = workspace
        .plan(
            &PreparedArgv {
                argv: argv(&root, tree, cov, prof),
                changed_files: None,
            },
            &grants,
        )
        .unwrap();
    let (capture, receiver) = Capture::new(
        workspace,
        "rust-coverage.lcov",
        4 * 1024 * 1024,
        Arc::new(ArtifactBudget::new(16 * 1024 * 1024)),
    )
    .unwrap();
    let group = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
    let group_path = group.report().path.clone();
    let limits = Limits {
        timeout: std::time::Duration::from_secs(120),
        max_output_chars: 12000,
        cpu_seconds: Some(120),
        address_space_bytes: None,
    };
    let execution =
        confined_execution::start_with_resources(plan, group, limits, Box::new(capture)).unwrap();
    let report = tokio::time::timeout(std::time::Duration::from_secs(150), execution.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, Outcome::Exited, "{report:?}");
    assert_eq!(report.exit_code, Some(0), "{report:?}");
    assert!(report.cleanup_complete);
    let artifact = receiver.await.unwrap().unwrap();
    let source = crate::candidate_python_coverage::ChangedFile {
        path: "src/lib.rs".into(),
        absolute_path: String::new(),
        lines: [1, 2].into_iter().collect(),
        high_risk: false,
    };
    let measured = crate::candidate_rust_coverage::evaluate(
        &artifact.bytes,
        root.to_str().unwrap(),
        &[source],
        80.,
        80.,
    )
    .unwrap();
    assert!(measured.other.measurable > 0);
    assert!(measured.other_pass);
    assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), index_before);
    assert!(!scratch.exists());
    assert!(!group_path.exists());
}
