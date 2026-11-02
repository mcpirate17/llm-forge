use super::*;
use crate::candidate_source::{Candidate, Change, Kind};
use sha2::Digest;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const POLICY: &str = r#"schema_version = 1
block_at = "high"
max_workers = 2
cache_ttl_days = 7
claim_max_age_hours = 24
max_file_bytes = 100000
max_binary_bytes = 200000
coverage_threshold = 80.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2026-11-30
[classes]
governance = ["conductor/*"]
[risk]
high = ["conductor/candidate_review/*"]
[paths]
protected_deletes = []
hot = []
generated = ["generated/*"]
[checks.z_first]
kind = "builtin"
profiles = ["fast", "full"]
severity = "critical"
always = true
[checks.a_second]
kind = "builtin"
profiles = ["full", "nightly"]
"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-policy-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, body: &str) -> PathBuf {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }
    fn load(&self, explicit: Option<&str>) -> Result<Policy> {
        load::load(&self.0, explicit, None, "2026-11-01")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn make_candidate(changes: Vec<Change>) -> Candidate {
    Candidate {
        kind: Kind::Index,
        tree_oid: "a".repeat(40),
        base_tree_oid: "b".repeat(40),
        base_commit_oid: None,
        commit_oid: None,
        target_ref: None,
        changes,
        integration_base_oid: None,
        integration_base_detail: "unresolved".into(),
    }
}
fn change(path: &str, old: Option<&str>, new_mode: &str) -> Change {
    Change {
        status: "M".into(),
        path: path.into(),
        old_path: old.map(str::to_owned),
        old_mode: "100644".into(),
        new_mode: new_mode.into(),
        old_oid: "a".repeat(40),
        new_oid: "b".repeat(40),
        classes: vec!["prior".into()],
        risk: "prior".into(),
    }
}

#[test]
fn snapshot_config_precedence_raw_digest_and_check_order_are_preserved() {
    let fixture = Fixture::new();
    fixture.write(
        "pyproject.toml",
        "[tool.conductor]\ncandidate_policy='configured.toml'\n",
    );
    fixture.write("configured.toml", POLICY);
    fixture.write("env.toml", &format!("# env\n{POLICY}"));
    fixture.write("explicit.toml", &format!("# explicit\n{POLICY}"));
    let configured = fixture.load(None).unwrap();
    assert_eq!(configured.path, "configured.toml");
    assert_eq!(
        configured.digest,
        format!("{:x}", sha2::Sha256::digest(POLICY.as_bytes()))
    );
    let env = load::load(&fixture.0, None, Some(" env.toml "), "2026-11-01").unwrap();
    assert_eq!(env.path, "env.toml");
    assert_ne!(env.digest, configured.digest);
    assert_eq!(env.document, configured.document);
    let explicit = load::load(
        &fixture.0,
        Some("./explicit.toml"),
        Some("missing.toml"),
        "2026-11-01",
    )
    .unwrap();
    assert_eq!(explicit.path, "explicit.toml");
    assert_eq!(
        configured
            .active_checks("full")
            .unwrap()
            .iter()
            .map(|v| v["check_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["z_first", "a_second"]
    );
    assert_eq!(configured.active_checks("fast").unwrap().len(), 1);
    assert_eq!(
        configured.active_checks("nightly").unwrap()[0]["check_id"],
        "a_second"
    );
    assert!(configured.active_checks("unknown").is_err());
    fixture.write(
        "forge.toml",
        "schema_version=1\n[conductor]\ncandidate_policy='explicit.toml'\n",
    );
    assert_eq!(fixture.load(None).unwrap().path, "explicit.toml");
    fixture.write("forge.toml", "not valid TOML");
    assert!(fixture.load(None).is_err());
    assert!(fixture.load(Some("explicit.toml")).is_ok());
}

#[test]
fn ordered_tools_and_literal_calendar_date_conversion_match_python() {
    let tools = r#"
[tools.z_first]
executable="z"
version_command=["z","--version"]
expected_version="1"
required_profiles=["full"]
provided_by="fixture"
rationale="fixture"
[tools.a_second]
executable="a"
version_command=[]
expected_version="1"
required_profiles=[]
provided_by="fixture"
rationale="fixture"
"#;
    let raw = format!(
        "{}{tools}",
        POLICY.replace("2026-11-30", "2026-11-30T23:59:00-12:00")
    );
    let fixture = Fixture::new();
    fixture.write("policy.toml", &raw);
    let policy = fixture.load(Some("policy.toml")).unwrap();
    assert_eq!(policy.document["baseline_expires"], "2026-11-30");
    assert_eq!(policy.document["tools"][0]["tool_id"], "z_first");
    assert_eq!(policy.document["tools"][1]["tool_id"], "a_second");
    for raw in [
        "value = 12:30:00",
        "value = nan",
        "value = inf",
        "value = -inf",
    ] {
        assert!(normalize::parse(raw.as_bytes()).is_err());
    }
}

#[test]
fn confined_policy_and_config_symlinks_allow_only_relative_internal_targets() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fixture.write("rules/real.toml", POLICY);
    outside.write("outside.toml", POLICY);
    symlink("rules/real.toml", fixture.0.join("policy.toml")).unwrap();
    assert!(fixture.load(Some("policy.toml")).is_ok());
    symlink(
        outside.0.join("outside.toml"),
        fixture.0.join("escape.toml"),
    )
    .unwrap();
    assert!(fixture.load(Some("escape.toml")).is_err());
    let relative_escape = PathBuf::from("..")
        .join(outside.0.file_name().unwrap())
        .join("outside.toml");
    symlink(relative_escape, fixture.0.join("relative-escape.toml")).unwrap();
    assert!(fixture.load(Some("relative-escape.toml")).is_err());
    symlink(
        fixture.0.join("rules/real.toml"),
        fixture.0.join("absolute-internal.toml"),
    )
    .unwrap();
    assert!(fixture.load(Some("absolute-internal.toml")).is_err());
    symlink("rules", fixture.0.join("root-link")).unwrap();
    assert!(load::load(
        &fixture.0.join("root-link"),
        Some("real.toml"),
        None,
        "2026-11-01"
    )
    .is_err());
    fixture.write(
        "config/real.toml",
        "[tool.conductor]\ncandidate_policy='policy.toml'\n",
    );
    symlink("config/real.toml", fixture.0.join("pyproject.toml")).unwrap();
    assert!(fixture.load(None).is_ok());
    symlink(outside.0.join("outside.toml"), fixture.0.join("forge.toml")).unwrap();
    assert!(fixture.load(None).is_err());
    symlink("missing.toml", fixture.0.join("dangling.toml")).unwrap();
    assert!(fixture.load(Some("dangling.toml")).is_err());
    for path in ["../outside.toml", "/absolute.toml", ".", "a/../policy.toml"] {
        assert!(fixture.load(Some(path)).is_err());
    }
}

#[test]
fn snapshot_traversal_checks_every_physical_ancestor() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.write("nested/policy.toml", POLICY);
    symlink("nested", fixture.0.join("nested-link")).unwrap();
    for path in [
        fixture.0.join("nested-link"),
        fixture.0.join("nested-link/../nested"),
        fixture.0.join("missing/../nested"),
    ] {
        assert!(load::load(&path, Some("policy.toml"), None, "2026-11-01").is_err());
    }
    assert!(load::load(
        &fixture.0.join("nested/../nested"),
        Some("policy.toml"),
        None,
        "2026-11-01"
    )
    .is_ok());
}

#[test]
fn dates_expiry_and_exact_waiver_metadata_are_preserved() {
    let fixture = Fixture::new();
    let exceptions = r#"
[[exceptions]]
id="today"
check="z_first"
path="src/one.rs"
owner="Tim"
justification="specific fixture exception justification"
expires=2026-11-01
[[exceptions]]
id="expired"
check="z_first"
path="src/two.rs"
owner="Tim"
justification="specific fixture exception justification"
expires=2026-10-31
[[value_waivers]]
integration_base="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
nodeids=["tests/test_case.py::test_exact"]
reason="specific fixture"
approved_by="Tim"
approved_on=2026-10-01
expires=2026-11-01
"#;
    fixture.write("policy.toml", &format!("{POLICY}{exceptions}"));
    let policy = fixture.load(Some("policy.toml")).unwrap();
    assert_eq!(policy.document["exceptions"][0]["exception_id"], "today");
    assert_eq!(
        policy.document["expired_exceptions"][0]["exception_id"],
        "expired"
    );
    assert_eq!(
        policy.document["value_waivers"][0]["nodeids"],
        json!(["tests/test_case.py::test_exact"])
    );
    assert_eq!(policy.document["value_waivers"][0]["expires"], "2026-11-01");
    assert!(!policy.digest.is_empty());
    assert!(validate_today("2024-02-29").is_ok());
    for today in [
        "2026-02-29",
        "1900-02-29",
        "0000-01-01",
        "2026-00-01",
        "2026-01-00",
        "2026-04-31",
        "2026-1-01",
        "garbage",
    ] {
        assert!(validate_today(today).is_err());
    }
    assert!(load::load(&fixture.0, Some("policy.toml"), None, "2026-12-01").is_err());
}

#[test]
fn classification_keeps_rename_sides_deleted_modes_and_candidate_identity() {
    let fixture = Fixture::new();
    fixture.write("policy.toml", POLICY);
    let policy = fixture.load(Some("policy.toml")).unwrap();
    let mut candidate = make_candidate(vec![
        change(
            "generated/new.py",
            Some("conductor/candidate_review/old.rs"),
            "100644",
        ),
        change("old.sh", None, "000000"),
    ]);
    let tree = candidate.tree_oid.clone();
    policy.classify_candidate(&mut candidate).unwrap();
    assert_eq!(candidate.tree_oid, tree);
    let renamed = &candidate.changes[0];
    for class in [
        "generated",
        "governance",
        "native",
        "python",
        "rust",
        "source",
    ] {
        assert!(renamed.classes.iter().any(|value| value == class));
    }
    assert_eq!(renamed.risk, "high");
    assert!(candidate.changes[1].deleted());
    assert!(candidate.changes[1]
        .classes
        .iter()
        .any(|value| value == "shell"));
    let mut malformed = make_candidate(vec![change("../escape", None, "100644")]);
    assert!(policy.classify_candidate(&mut malformed).is_err());
    assert_eq!(malformed.changes[0].classes, vec!["prior"]);
}

#[test]
fn missing_malformed_and_resource_excesses_fail_without_fallback_or_partial_classification() {
    let fixture = Fixture::new();
    assert!(fixture.load(Some("missing.toml")).is_err());
    for raw in [vec![0xff], vec![b' '; MAX_BYTES + 1]] {
        std::fs::write(fixture.0.join("policy.toml"), raw).unwrap();
        assert!(fixture.load(Some("policy.toml")).is_err());
    }
    assert!(
        normalize::parse(format!("value='{}'", "x".repeat(MAX_STRING + 1)).as_bytes()).is_err()
    );
    assert!(normalize::parse(
        format!(
            "value={}0{}",
            "[".repeat(MAX_DEPTH + 2),
            "]".repeat(MAX_DEPTH + 2)
        )
        .as_bytes()
    )
    .is_err());
    assert!(normalize::parse(format!("value=[{}]", "0,".repeat(MAX_NODES)).as_bytes()).is_err());
    fixture.write("policy.toml", POLICY);
    let mut policy = fixture.load(Some("policy.toml")).unwrap();
    policy.document["class_globs"] = json!({"docs":vec!["?".repeat(MAX_STRING); 25]});
    let mut candidate = make_candidate(vec![change(&"a".repeat(1024), None, "100644")]);
    assert!(policy
        .classify_candidate(&mut candidate)
        .unwrap_err()
        .to_string()
        .contains("work limit"));
    assert_eq!(candidate.changes[0].classes, vec!["prior"]);
}
