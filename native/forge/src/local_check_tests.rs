use super::*;

const POLICY: &str = r#"
base_ref = "origin/main"
allowed_untracked_prefixes = ["research/"]

[[setup]]
name = "sync"
run = "true"
timeout_s = 2

[[check]]
name = "python"
run = "true"
paths = ['^src/']
timeout_s = 2
"#;

#[test]
fn policy_rejects_invalid_regex_duplicate_and_unbounded_steps() {
    assert!(Policy::parse(POLICY).is_ok());
    assert!(Policy::parse(&POLICY.replace("'^src/'", "'('")).is_err());
    assert!(Policy::parse(&POLICY.replace("timeout_s = 2", "timeout_s = 0")).is_err());
    assert!(Policy::parse(&POLICY.replace("name = \"python\"", "name = \"sync\"")).is_err());
    assert!(Policy::parse(&POLICY.replace("research/", "../")).is_err());
}

#[test]
fn shipped_policy_parses_and_keeps_baseline_setup_with_its_check() {
    let policy = Policy::parse(include_str!("../../../.forge/local-check.toml")).unwrap();
    let changed = vec!["native/conductor-native/src/lib.rs".to_string()];
    let tools = policy
        .setup
        .iter()
        .find(|step| step.name == "baseline-tools")
        .unwrap();
    let baselines = policy
        .checks
        .iter()
        .find(|step| step.name == "candidate-baselines")
        .unwrap();
    assert!(selected(baselines, &changed));
    assert!(selected(tools, &changed));
    assert_eq!(policy.checks.len(), 10);
}

#[test]
fn infrastructure_changes_force_full_selection() {
    assert!(!effective_all(false, &["docs/readme.md".into()]));
    assert!(effective_all(
        false,
        &["native/forge/src/local_check.rs".into()]
    ));
    assert!(effective_all(false, &[".forge/local-check.toml".into()]));
    assert!(effective_all(true, &[]));
}

#[test]
fn a_second_check_cannot_take_the_same_lock() {
    let dir = std::env::temp_dir().join(format!("forge-check-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let first = acquire_lock(&dir, true).unwrap();
    assert!(acquire_lock(&dir, true).is_err());
    assert!(acquire_lock(&dir, false).is_err());
    drop(first);
    assert!(acquire_lock(&dir, false).is_ok());
    fs::remove_dir_all(dir).unwrap();
}
