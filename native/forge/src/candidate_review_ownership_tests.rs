use super::*;
use crate::candidate_snapshot::Limits;
use crate::candidate_source::Kind;
use std::{
    fs,
    path::{Path, PathBuf},
};
const TODAY: &str = "2026-11-01";
const POLICY: &str = r#"schema_version=1
block_at='high'
max_workers=2
cache_ttl_days=7
claim_max_age_hours=24
max_file_bytes=100000
max_binary_bytes=200000
coverage_threshold=80.0
high_risk_coverage_threshold=90.0
baseline_expires=2026-11-30
[classes]
python=['**/*.py']
docs=['docs/**','*.md']
config=['*.toml','*.yml']
[risk]
high=[]
[paths]
protected_deletes=[]
hot=[]
generated=[]
[checks.ownership]
kind='builtin'
profiles=['fast','full']
always=true
severity='low'
"#;
struct Fixture {
    root: PathBuf,
    repo: git2::Repository,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::native_id::new("review-ownership").unwrap());
        fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(root.join("repo")).unwrap();
        let fixture = Self { root, repo };
        fixture.write("policy.toml", POLICY);
        fixture.commit();
        fixture
    }
    fn write(&self, path: &str, text: &str) {
        let output = self.root.join("repo").join(path);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(output, text).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn commit(&self) {
        let tree = self.repo.index().unwrap().write_tree().unwrap();
        let tree = self.repo.find_tree(tree).unwrap();
        let signature = git2::Signature::now("test", "test@example.invalid").unwrap();
        let head = self
            .repo
            .head()
            .ok()
            .map(|head| head.peel_to_commit().unwrap());
        self.repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "fixture",
                &tree,
                &head.iter().collect::<Vec<_>>(),
            )
            .unwrap();
    }
    fn captured(&self) -> (Source, Snapshot, Candidate, Policy) {
        let source = Source::open(&self.root.join("repo")).unwrap();
        let candidate = source.resolve(Kind::Index, "HEAD", None).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &candidate.tree_oid,
            &self.root.join(crate::native_id::new("snapshot").unwrap()),
            Limits::default(),
        )
        .unwrap();
        let policy =
            crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), TODAY).unwrap();
        (source, snapshot, candidate, policy)
    }
    fn state(&self, name: &str, payload: &str) {
        let root = self.repo.path().join("governance");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(name), payload).unwrap();
    }
    fn claims(&self, claims: &[Value]) {
        self.state(
            "ownership-claims.json",
            &json!({"schema_version":1,"claims":claims}).to_string(),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn claim(owner: &str, paths: &[&str], now: f64, created: f64, expected: f64, expiry: f64) -> Value {
    let fields = json!({"owner":owner,"paths":paths,"justification":"bounded test",
        "created_at":crate::instant::isoformat_utc(now+created),
        "expected_at":crate::instant::isoformat_utc(now+expected),
        "expires_at":crate::instant::isoformat_utc(now+expiry)});
    let digest = ownership::sha256_json(&fields);
    let mut result = fields;
    result["claim_id"] = json!(format!("claim-{}", &digest[..20]));
    result
}
fn context<'a>(
    source: &'a Source,
    snapshot: &'a Snapshot,
    candidate: &'a Candidate,
    policy: &'a Policy,
    owner: &'a str,
) -> Context<'a> {
    Context {
        source,
        snapshot,
        candidate,
        policy,
        today: TODAY,
        surface: Surface::PreCommit,
        owner,
    }
}

#[test]
fn own_claim_covers_descendants_casefolds_owner_and_preserves_store_digest() {
    let f = Fixture::new();
    f.write("src/core/a.py", "value=1\n");
    let now = crate::instant::now();
    let own = claim("Lane-A", &["src/core"], now, -60.0, 1800.0, 3600.0);
    f.claims(std::slice::from_ref(&own));
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect_at(
        context(&source, &snapshot, &candidate, &policy, "lane-a"),
        now,
    )
    .unwrap();
    assert_eq!(evidence.result.status, CheckStatus::Passed);
    assert_eq!(
        evidence.result.metrics["claim_ids"],
        json!([own["claim_id"]])
    );
    assert_eq!(
        evidence.result.metrics["state_sha256"],
        ownership::load_claims(source.root()).unwrap().1
    );
    assert_eq!(evidence.evidence["approval"], false);
    evidence.verify().unwrap();
}

#[test]
fn absent_or_wrong_owner_claims_emit_fixed_high_rules_even_with_low_policy() {
    for wrong_owner in [false, true] {
        let f = Fixture::new();
        f.write("src/a.py", "value=1\n");
        let now = crate::instant::now();
        if wrong_owner {
            f.claims(&[claim("lane-b", &["src/a.py"], now, -60.0, 1800.0, 3600.0)]);
        }
        let (source, snapshot, candidate, policy) = f.captured();
        let evidence = collect_at(
            context(&source, &snapshot, &candidate, &policy, "lane-a"),
            now,
        )
        .unwrap();
        assert_eq!(evidence.result.findings.len(), 1);
        assert_eq!(
            evidence.result.findings[0].rule_id,
            if wrong_owner {
                "claim-owner-mismatch"
            } else {
                "unclaimed-path"
            }
        );
        assert_eq!(
            evidence.result.findings[0].severity,
            crate::candidate_assessment::Severity::High
        );
        assert_eq!(evidence.result.findings[0].fingerprint.len(), 24);
    }
}

#[test]
fn overlap_is_directional_and_an_own_exact_claim_can_coexist_with_foreign_ancestor() {
    for scope in ["src/cor", "src/core/a.py/deeper", "src/core/a.py"] {
        let f = Fixture::new();
        f.write("src/core/a.py", "value=1\n");
        let now = crate::instant::now();
        let mut claims = vec![claim("lane-a", &[scope], now, -60.0, 1800.0, 3600.0)];
        if scope == "src/core/a.py" {
            claims.push(claim("lane-b", &["src/core"], now, -60.0, 1800.0, 3600.0));
        }
        f.claims(&claims);
        let (source, snapshot, candidate, policy) = f.captured();
        let evidence = collect_at(
            context(&source, &snapshot, &candidate, &policy, "lane-a"),
            now,
        )
        .unwrap();
        assert_eq!(
            evidence.result.status,
            if scope == "src/core/a.py" {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            }
        );
        if scope != "src/core/a.py" {
            assert_eq!(evidence.result.findings[0].rule_id, "unclaimed-path");
        }
    }
}

#[test]
fn expired_claims_are_stale_and_live_activity_deadlines_are_rechecked() {
    let f = Fixture::new();
    f.write("src/a.py", "value=1\n");
    let now = crate::instant::now();
    let expired = claim("lane-a", &["src/a.py"], now, -7200.0, -5400.0, -3600.0);
    f.claims(&[expired]);
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect_at(
        context(&source, &snapshot, &candidate, &policy, "lane-a"),
        now,
    )
    .unwrap();
    assert_eq!(
        evidence
            .result
            .findings
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect::<Vec<_>>(),
        ["stale-claim", "unclaimed-path"]
    );
    drop(evidence);
    let active = claim("lane-a", &["src/a.py"], now, -3600.0, -60.0, 3600.0);
    f.claims(std::slice::from_ref(&active));
    let seen = std::collections::BTreeMap::from([(
        active["claim_id"].as_str().unwrap(),
        crate::instant::isoformat_utc(now - 300.0),
    )]);
    f.state(
        "claim-activity.json",
        &json!({"schema_version":1,"seen":seen}).to_string(),
    );
    let evidence = collect_at(
        context(&source, &snapshot, &candidate, &policy, "lane-a"),
        now,
    )
    .unwrap();
    assert_eq!(evidence.result.status, CheckStatus::Passed);
    evidence.verify_at(now + 299.0).unwrap();
    assert!(evidence.verify_at(now + 301.0).is_err());
}

#[test]
fn malformed_claim_identity_or_activity_yields_malformed_store_rule() {
    for activity in [false, true] {
        let f = Fixture::new();
        f.write("src/a.py", "value=1\n");
        let now = crate::instant::now();
        let mut own = claim("lane-a", &["src/a.py"], now, -60.0, 1800.0, 3600.0);
        if activity {
            f.state("claim-activity.json", "{malformed");
        } else {
            own["owner"] = json!("forged-after-hash");
        }
        f.claims(&[own]);
        let (source, snapshot, candidate, policy) = f.captured();
        let evidence = collect_at(
            context(&source, &snapshot, &candidate, &policy, "lane-a"),
            now,
        )
        .unwrap();
        assert_eq!(evidence.result.findings[0].rule_id, "malformed-claim-store");
        evidence.verify_at(now).unwrap();
    }
}

#[test]
fn non_precommit_surfaces_do_not_consume_live_claim_store() {
    let f = Fixture::new();
    f.write("src/a.py", "value=1\n");
    f.state("ownership-claims.json", "malformed");
    let (source, snapshot, candidate, policy) = f.captured();
    for surface in [Surface::Manual, Surface::Ci, Surface::PostCommit] {
        let mut ctx = context(&source, &snapshot, &candidate, &policy, "lane-a");
        ctx.surface = surface;
        let evidence = collect(ctx).unwrap();
        assert_eq!(evidence.result.status, CheckStatus::Passed);
        assert_eq!(evidence.result.files, ["src/a.py"]);
        assert_eq!(evidence.result.metrics["status"], "local-precommit-only");
        assert!(evidence.host.is_none());
        evidence.verify().unwrap();
    }
}

#[test]
fn docs_config_current_work_and_deletions_are_exempt_from_coverage() {
    let f = Fixture::new();
    f.write("src/gone.py", "value=1\n");
    f.commit();
    fs::remove_file(f.root.join("repo/src/gone.py")).unwrap();
    let mut index = f.repo.index().unwrap();
    index.remove_path(Path::new("src/gone.py")).unwrap();
    index.write().unwrap();
    f.write("docs/readme.md", "documentation\n");
    f.write("host.yml", "config: true\n");
    f.write(".current_work.md", "lane status\n");
    f.state("ownership-claims.json", "malformed");
    let (source, snapshot, mut candidate, policy) = f.captured();
    for change in &mut candidate.changes {
        change.classes = vec!["python".into()];
    }
    let evidence = collect(context(&source, &snapshot, &candidate, &policy, "lane-a")).unwrap();
    assert!(evidence.result.files.is_empty());
    assert!(evidence.result.findings.is_empty());
}

#[test]
fn held_store_activity_policy_and_output_drift_fail_reverification() {
    for target in [
        "ownership-claims.json",
        "claim-activity.json",
        "policy.toml",
    ] {
        let f = Fixture::new();
        f.write("src/a.py", "value=1\n");
        let now = crate::instant::now();
        let own = claim("lane-a", &["src/a.py"], now, -60.0, 1800.0, 3600.0);
        f.claims(&[own]);
        f.state("claim-activity.json", "{\"schema_version\":1,\"seen\":{}}");
        let (source, snapshot, candidate, policy) = f.captured();
        let mut evidence = collect_at(
            context(&source, &snapshot, &candidate, &policy, "lane-a"),
            now,
        )
        .unwrap();
        evidence.evidence["approval"] = json!(true);
        assert!(evidence.verify_at(now).is_err());
        evidence.evidence["approval"] = json!(false);
        evidence.verify_at(now).unwrap();
        let path = if target == "policy.toml" {
            snapshot.path().join(target)
        } else {
            f.repo.path().join("governance").join(target)
        };
        fs::write(path, "drift").unwrap();
        assert!(evidence.verify_at(now).is_err(), "{target}");
    }
}

#[test]
fn absent_live_state_cannot_appear_after_observation() {
    let f = Fixture::new();
    f.write("src/a.py", "value=1\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy, "lane-a")).unwrap();
    f.claims(&[]);
    assert!(evidence.verify().is_err());
}

#[test]
fn shared_repository_observations_preserve_strict_grant_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("src/a.py", "value=1\n");
    f.claims(&[]);
    let path = f.repo.path().join("governance");
    fs::set_permissions(f.repo.path(), fs::Permissions::from_mode(0o775)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o775)).unwrap();
    fs::set_permissions(
        path.join("ownership-claims.json"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    let (source, snapshot, candidate, policy) = f.captured();
    let observed = collect(context(&source, &snapshot, &candidate, &policy, "lane-a")).unwrap();
    observed.verify().unwrap();
    assert!(crate::candidate_inputs::DirectoryBinding::open(&path, None).is_err());
    fs::set_permissions(
        path.join("ownership-claims.json"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(observed.verify().is_err());
    assert!(collect(context(&source, &snapshot, &candidate, &policy, "lane-a")).is_err());
}
