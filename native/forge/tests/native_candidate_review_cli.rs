#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

fn fixture() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case
}
fn run(case: &Case, execute: bool) -> Value {
    let mut command = case.command_for("review", "index", &[]);
    command.args(["--surface", "manual"]);
    if execute {
        command.arg("--execute");
    }
    value(
        forge::process_probe::run(&mut command, Duration::from_secs(20), 1024 * 1024).unwrap(),
        u8::from(execute).into(),
    )
}
#[test]
fn dry_run_names_implicit_obligations_without_executing() {
    let case = fixture();
    let before = case.source_state();
    let report = run(&case, false);
    assert_eq!(report["executed"], false);
    assert_eq!(report["review_complete"], false);
    assert_eq!(
        report["required_checks"],
        json!(["engine-integrity", "zeta", "alpha", "attestation"])
    );
    assert!(report["receipt_digest"].is_null());
    case.assert_preserved(&before);
}
#[test]
fn unsupported_required_check_cannot_be_a_complete_review() {
    let case = fixture();
    case.staged_policy(|text| {
        text.replace("classes=['rust']", "classes=['cuda']")
            .replace(
                "[checks.alpha]\nkind='builtin'",
                "[checks.alpha]\nkind='builtin'\nalways=true",
            )
    });
    let before = case.source_state();
    let report = run(&case, true);
    assert_eq!(report["decision"], "fail");
    assert_eq!(report["review_complete"], false);
    assert_eq!(report["approval"], false);
    assert!(report["incomplete"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["check_id"] == "alpha"));
    let receipt =
        forge::candidate_receipt::parse_and_verify(&serde_json::to_vec(&report).unwrap()).unwrap();
    for format in [
        forge::candidate_report::Format::Json,
        forge::candidate_report::Format::Human,
        forge::candidate_report::Format::Sarif,
        forge::candidate_report::Format::Junit,
    ] {
        assert!(!forge::candidate_report::render(&receipt, format)
            .unwrap()
            .is_empty());
    }
    case.assert_preserved(&before);
}
#[test]
fn complete_review_does_not_accept_partial_check_selection() {
    let case = fixture();
    let mut command = case.command_for("review", "index", &["zeta"]);
    command.args(["--surface", "manual"]);
    assert!(
        !forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024)
            .unwrap()
            .status
            .success()
    );
}

fn capture_engine(case: &Case) {
    let build_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repository_root = case.fixture.root.join("repo");
    for path in forge::source_fingerprint::files(&build_root).unwrap() {
        let destination = repository_root.join(&path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, std::fs::read(build_root.join(&path)).unwrap()).unwrap();
        case.fixture.add(path.to_str().unwrap());
    }
    case.staged_policy(|policy| policy.replace("[checks.zeta]", "[checks.zeta]\nalways=true"));
    let repository = git2::Repository::open(&repository_root).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(tree).unwrap();
    let author = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &author,
            &author,
            "captured engine",
            &tree,
            &[&parent],
        )
        .unwrap();
    case.fixture
        .stage("readme.md", "documentation-only candidate\n");
}

#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and current source-matching FORGE_TEST_BIN"]
fn complete_native_review_executes_owned_stages_and_publishes_bound_receipt() {
    let mut case = fixture();
    case.delegated();
    capture_engine(&case);
    let before = case.source_state();
    let receipt_path = case.fixture.root.join("review.json");
    let mut command = case.command_for("review", "index", &[]);
    command
        .args(["--surface", "manual", "--execute", "--receipt"])
        .arg(&receipt_path);
    let report = value(
        forge::process_probe::run(&mut command, Duration::from_secs(40), 1024 * 1024).unwrap(),
        0,
    );
    assert_eq!(report["decision"], "pass");
    assert_eq!(report["review_complete"], true);
    assert_eq!(report["execution_verified"], true);
    assert_eq!(report["approval"], false);
    assert_eq!(report["incomplete"], json!([]));
    assert_eq!(report["checks"][0]["status"], "passed");
    assert_eq!(report["checks"][1]["status"], "passed");
    let stages = report["stages"][0]["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert!(stages
        .iter()
        .all(|stage| stage["report"]["cleanup_complete"] == true
            && stage["report"]["resources"]["verified"] == true));
    let published =
        forge::candidate_receipt::parse_and_verify(&std::fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(published.payload(), &report);
    case.assert_preserved(&before);
}

#[test]
fn failed_fresh_review_exports_its_own_sealed_json_sarif_and_junit() {
    let case = fixture();
    case.staged_policy(|text| {
        text.replace("classes=['rust']", "classes=['cuda']")
            .replace(
                "[checks.alpha]\nkind='builtin'",
                "[checks.alpha]\nkind='builtin'\nalways=true",
            )
    });
    let before = case.source_state();
    let receipt = case.fixture.root.join("review.json");
    let sarif = case.fixture.root.join("review.sarif");
    let junit = case.fixture.root.join("review.xml");
    // A prior artifact must not supply the newly requested exports.
    std::fs::write(&receipt, b"stale receipt bytes").unwrap();
    let mut command = case.command_for("review", "index", &[]);
    command
        .args(["--surface", "manual", "--execute", "--receipt"])
        .arg(&receipt)
        .arg("--sarif-out")
        .arg(&sarif)
        .arg("--junit-out")
        .arg(&junit);
    let report = value(
        forge::process_probe::run(&mut command, Duration::from_secs(20), 1024 * 1024).unwrap(),
        1,
    );
    assert_eq!(report["decision"], "fail");
    assert_eq!(report["review_complete"], false);
    let published =
        forge::candidate_receipt::parse_and_verify(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(published.payload(), &report);
    let actual_sarif: Value = serde_json::from_slice(&std::fs::read(&sarif).unwrap()).unwrap();
    assert_eq!(actual_sarif["version"], "2.1.0");
    assert!(!actual_sarif["runs"][0]["results"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read(&sarif).unwrap(),
        forge::candidate_report::render(&published, forge::candidate_report::Format::Sarif)
            .unwrap()
    );
    assert_eq!(
        std::fs::read(&junit).unwrap(),
        forge::candidate_report::render(&published, forge::candidate_report::Format::Junit)
            .unwrap()
    );
    case.assert_preserved(&before);
}

#[test]
fn review_exports_require_execution_and_refuse_colliding_destinations_before_writes() {
    let case = fixture();
    let output = case.fixture.root.join("review.json");
    std::fs::write(&output, b"preserve this old artifact").unwrap();
    let mut command = case.command_for("review", "index", &[]);
    command
        .args(["--surface", "manual", "--sarif-out"])
        .arg(&output);
    assert!(
        !forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024)
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"preserve this old artifact"
    );
    let mut command = case.command_for("review", "index", &[]);
    command
        .args(["--surface", "manual", "--execute", "--receipt"])
        .arg(&output)
        .arg("--sarif-out")
        .arg(case.fixture.root.join(".").join("review.json"));
    let result =
        forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("review output paths must be distinct")
    );
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"preserve this old artifact"
    );
}
