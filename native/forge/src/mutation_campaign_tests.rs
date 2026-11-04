use super::*;
use crate::mutation_run_test_support::{campaign, manifest, write};
use serde_json::json;

fn refusal(overrides: Value) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    Campaign::load(&manifest(dir.path(), overrides)).expect_err("refused")
}

#[test]
fn a_mutant_is_named_by_what_it_does_not_by_where_it_sits() {
    let names = identify(&[
        ["a.py", "arith", "x + 1", "x - 1"],
        ["a.py", "arith", "x + 1", "x * 1"],
        ["b.py", "arith", "x + 1", "x - 1"],
    ]);

    assert_ne!(names[0], names[1]);
    assert_ne!(names[0], names[2]);
    assert!(names[0].starts_with("arith-"), "{}", names[0]);
    // Moving a mutant within its file leaves its name alone: the line is no input.
    assert_eq!(
        names[0],
        identify(&[["a.py", "arith", "x + 1", "x - 1"]])[0]
    );
}

#[test]
fn identical_rewrites_in_one_file_get_distinct_names() {
    let entry = ["a.py", "arith", "x + 1", "x - 1"];

    let names = identify(&[entry, entry, entry]);

    assert!(names[0].ends_with("-0"), "{}", names[0]);
    assert!(names[1].ends_with("-1"), "{}", names[1]);
    assert!(names[2].ends_with("-2"), "{}", names[2]);
}

#[test]
fn the_per_mutant_bound_comes_from_the_manifest_or_the_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let generator = json!({
        "source": ["src/a.py"], "run_timeout_seconds": 60, "mutant_timeout_seconds": 7,
    });
    let mut pinned_bound = campaign(dir.path(), json!({"generator": generator}));
    assert_eq!(pinned_bound.resolve_mutant_timeout(1000.0), 7);
    assert_eq!(pinned_bound.mutant_timeout(), Ok(7));

    let mut floor = campaign(dir.path(), json!({}));
    assert!(floor.mutant_timeout().is_err());
    assert_eq!(floor.resolve_mutant_timeout(1.0), 60);

    let mut scaled = campaign(dir.path(), json!({}));
    assert_eq!(scaled.resolve_mutant_timeout(40.2), 121);
    assert_eq!(scaled.mutant_timeout(), Ok(121));
}

#[test]
fn a_hand_rolled_or_foreign_manifest_is_refused() {
    assert!(refusal(json!({"mutation_engine": "stryker"})).contains("not a generated engine"));
    assert!(refusal(json!({"title": 3})).contains("'title' must be str, got int"));
    assert!(refusal(json!({"generator": {"source": ["a"]}}))
        .contains("missing required key 'run_timeout_seconds'"));
    assert!(refusal(json!({"environment": {"A": 1}})).contains("'environment' must hold strings"));
}

#[test]
fn a_manifest_must_pin_what_it_will_mutate_and_run() {
    let generator = json!({"source": [], "run_timeout_seconds": 60});
    assert!(refusal(json!({"generator": generator})).contains("at least one glob"));
    assert!(refusal(json!({"source_sha256": {}})).contains("must pin every mutated file"));
    assert!(refusal(json!({"test_sha256": {}})).contains("must pin every test file"));
}

#[test]
fn a_campaign_vouches_only_for_tests_it_executed() {
    let pin = "0".repeat(64);
    let error = refusal(json!({
        "test_sha256": {"tests/test_a.py": pin, "tests/test_b.py": pin},
    }));

    assert!(
        error.contains("names some declared tests but not ['tests/test_b.py']"),
        "{error}"
    );
}

#[test]
fn a_recorded_flag_defaults_from_the_baseline_a_legacy_manifest_holds() {
    let dir = tempfile::tempdir().expect("tempdir");

    assert!(!campaign(dir.path(), json!({})).survivor_baseline_recorded);
    assert!(
        campaign(dir.path(), json!({"survivor_baseline": ["b", "a"]})).survivor_baseline_recorded
    );
    assert_eq!(
        campaign(dir.path(), json!({"survivor_baseline": ["b", "a"]})).survivor_baseline,
        ["a", "b"]
    );
}

#[test]
fn drift_reports_changed_and_absent_sources_and_tests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write(root, "src/a.py", "a = 1\n");
    write(root, "tests/test_a.py", "b = 1\n");
    let pins = |relative: &str, body: &str| json!({relative: sha256_hex(body.as_bytes())});
    let subject = campaign(
        root,
        json!({
            "source_sha256": pins("src/a.py", "a = 1\n"),
            "test_sha256": pins("tests/test_a.py", "b = 1\n"),
        }),
    );
    assert!(drift(&subject, root).expect("hashes").is_empty());
    assert!(require_no_drift(&subject, root).is_ok());

    write(root, "src/a.py", "a = 2\n");
    fs::remove_file(root.join("tests/test_a.py")).expect("remove");

    let drifted = drift(&subject, root).expect("hashes");
    assert_eq!(drifted["src/a.py"], sha256_hex(b"a = 2\n"));
    assert_eq!(drifted["tests/test_a.py"], "absent");
    let error = require_no_drift(&subject, root).expect_err("refused");
    assert!(
        error.starts_with("snapshot source hashes drifted"),
        "{error}"
    );
}

#[test]
fn a_bare_python_argv_resolves_to_the_pinned_interpreter() {
    let argv = |head: &str| vec![head.to_string(), "-m".to_string(), "pytest".to_string()];

    assert_eq!(
        pinned(&argv("python"), "/venv/bin/python")[0],
        "/venv/bin/python"
    );
    assert_eq!(
        pinned(&argv("python3"), "/venv/bin/python")[0],
        "/venv/bin/python"
    );
    assert_eq!(pinned(&argv("cargo"), "/venv/bin/python")[0], "cargo");
    assert!(pinned(&[], "/venv/bin/python").is_empty());
}

#[test]
fn a_survivor_baseline_is_recorded_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut subject = campaign(dir.path(), json!({}));

    let survivors = ["z".to_string(), "a".to_string()];
    assert_eq!(record_survivor_baseline(&mut subject, &survivors), Ok(true));
    assert_eq!(subject.survivor_baseline, ["a", "z"]);
    let hash = subject.manifest_sha256.clone();
    assert_eq!(sha256_path(&subject.manifest_path), Ok(hash.clone()));

    assert_eq!(
        record_survivor_baseline(&mut subject, &["new".to_string()]),
        Ok(false)
    );
    assert_eq!(subject.survivor_baseline, ["a", "z"]);
    assert_eq!(subject.manifest_sha256, hash);
}

#[test]
fn only_a_real_option_value_reads_as_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let generator = json!({
        "source": ["src/a.py"],
        "run_timeout_seconds": 60,
        "options": {"name": "x", "empty": "", "version": 18, "flag": true},
    });
    let subject = campaign(dir.path(), json!({"generator": generator}));

    assert_eq!(subject.option_text("name").as_deref(), Some("x"));
    assert_eq!(subject.option_text("version").as_deref(), Some("18"));
    assert_eq!(subject.option_text("empty"), None);
    assert_eq!(subject.option_text("flag"), None);
    assert_eq!(subject.option_text("absent"), None);
}
