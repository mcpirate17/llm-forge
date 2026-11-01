//! Current-only native candidate costs. No interpreter, external Git, or model.
use forge::{
    candidate_receipt, candidate_snapshot,
    candidate_source::{Kind, Source},
};
use std::{hint::black_box, path::PathBuf, time::Instant};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn measure(name: &str, iterations: u32, mut operation: impl FnMut()) {
    for _ in 0..100 {
        operation();
    }
    let mut batches = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        batches.push(start.elapsed().as_nanos() as f64 / f64::from(iterations));
    }
    batches.sort_by(f64::total_cmp);
    println!(
        "{name}: median_ns={:.2} p95_ns={:.2} batches=31 iterations={iterations}",
        batches[15], batches[29]
    );
}

fn main() {
    let root = std::env::temp_dir().join(forge::resource_id("forge-candidate-bench").unwrap());
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let repository = git2::Repository::init(fixture.0.join("repo")).unwrap();
    std::fs::write(fixture.0.join("repo/file"), "fn example() {}\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(std::path::Path::new("file")).unwrap();
    index.write().unwrap();
    let tree = index.write_tree().unwrap().to_string();
    let source = Source::open(&fixture.0.join("repo")).unwrap();
    let reference: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../tests/fixtures/candidate_receipt/reference.json"
    ))
    .unwrap();
    let receipt = serde_json::to_vec(&reference["digest_valid_failed_receipt"]).unwrap();
    let output = fixture.0.join("snapshot");
    measure("receipt_digest", 1000, || {
        black_box(candidate_receipt::parse_and_verify(black_box(&receipt)).unwrap());
    });
    measure("staged_candidate_one_file", 100, || {
        black_box(source.resolve(Kind::Index, "HEAD", None).unwrap());
    });
    measure("snapshot_preflight_one_file", 100, || {
        black_box(
            candidate_snapshot::preflight(
                &source,
                &tree,
                &output,
                candidate_snapshot::Limits::default(),
            )
            .unwrap(),
        );
    });
    classification();
    glob_matching();
    assessment();
}

fn classification() {
    use conductor_native::candidate_policy::{classify, PreparedClassifier};
    use serde_json::json;
    let globs = json!({"class_globs":{"rust":["native/*.rs"],"test":["native/*/test_*.rs","native/*_test.rs"],
        "docs":["docs/*.md"],"source":["native/*","src/*"]},"generated_globs":["native/generated/*"],"high_risk_globs":["native/security/*"]});
    let changes: Vec<_> = (0..128).map(|index| json!({"path":format!("native/module{index}/test_file.rs"),
        "old_path":format!("src/module{index}/test_file.rs"),"old_mode":"100644","new_mode":"100644"})).collect();
    let mut index = 0usize;
    measure("policy_stateless_change", 1000, || {
        black_box(classify(&changes[index % changes.len()], &globs).unwrap());
        index += 1;
    });
    let mut prepared = PreparedClassifier::new(&globs).unwrap();
    let mut index = 0usize;
    measure("policy_prepared_change", 1000, || {
        black_box(prepared.classify(&changes[index % changes.len()]).unwrap());
        index += 1;
    });
}

fn glob_matching() {
    use conductor_native::candidate_policy::{glob_match, GlobWorkspace, PreparedGlob};
    let cases = [
        ("src/模块/🦀.rs", "src/*/[🦀-🦞].rs"),
        ("docs/épreuve.md", "docs/[!x]*.md"),
        ("src/module/test_7.rs", "src/*/test_[0-9].rs"),
        ("src/module/test_x.rs", "src/*/test_[0-9].rs"),
        ("literal/[unclosed", "literal/[unclosed"),
        ("notes/].txt", "notes/[]].txt"),
    ];
    let prepared: Vec<_> = cases
        .iter()
        .map(|(_, pattern)| PreparedGlob::new(pattern))
        .collect();
    let mut workspace = GlobWorkspace::default();
    for ((path, pattern), glob) in cases.iter().zip(&prepared) {
        assert_eq!(glob_match(path, pattern), workspace.matches(path, glob));
    }
    let mut index = 0usize;
    measure("glob_stateless_match", 1000, || {
        let (path, pattern) = cases[index % cases.len()];
        black_box(glob_match(black_box(path), black_box(pattern)));
        index += 1;
    });
    let mut index = 0usize;
    measure("glob_prepared_match", 1000, || {
        let position = index % cases.len();
        black_box(workspace.matches(black_box(cases[position].0), black_box(&prepared[position])));
        index += 1;
    });
}

fn assessment() {
    use forge::{
        candidate_assessment::{assess, CheckResult},
        candidate_policy::Policy,
        candidate_source::{Candidate, Change, Kind},
    };
    use serde_json::json;
    let raw = json!({
        "schema_version":1, "block_at":"high", "max_workers":2,
        "cache_ttl_days":7, "claim_max_age_hours":24,
        "max_file_bytes":100000, "max_binary_bytes":200000,
        "coverage_threshold":80.0, "high_risk_coverage_threshold":90.0,
        "baseline_expires":"2026-12-01", "classes":{}, "risk":{"high":[]},
        "paths":{"protected_deletes":[],"hot":[],"generated":[]},
        "checks":{"lint":{"kind":"builtin","profiles":["fast"],"severity":"high","attribution":"diff"}},
        "exceptions":[
            {"id":"selected", "check":"lint", "rule":"style", "path":"src/module/file[0-3].rs", "owner":"bench", "justification":"A narrow benchmark exception with specific evidence", "expires":"2026-11-30"},
            {"id":"stale", "check":"lint", "path":"docs/reference/*.md", "owner":"bench", "justification":"A narrow benchmark exception with specific evidence", "expires":"2026-11-30"}
        ]
    });
    let policy = Policy {
        path: "policy.toml".into(),
        digest: "a".repeat(64),
        document: conductor_native::candidate_policy::parse_policy(&raw, "2026-11-01").unwrap(),
    };
    let changes: Vec<_> = (0..32)
        .map(|i| Change {
            status: if i == 0 { "R100" } else { "M" }.into(),
            path: format!("src/module/file{i}.rs"),
            old_path: (i == 0).then(|| "old/renamed.rs".into()),
            old_mode: "100644".into(),
            new_mode: "100644".into(),
            old_oid: "a".repeat(40),
            new_oid: "b".repeat(40),
            classes: vec!["rust".into()],
            risk: "normal".into(),
        })
        .collect();
    let candidate = Candidate {
        kind: Kind::Index,
        tree_oid: "c".repeat(40),
        base_tree_oid: "c".repeat(40),
        base_commit_oid: None,
        commit_oid: None,
        target_ref: None,
        changes,
        integration_base_oid: None,
        integration_base_detail: String::new(),
    };
    let findings: Vec<_> = (0..32).map(|i| json!({"check_id":"lint", "rule_id":"style", "severity":"medium",
        "message":format!("finding {i}"), "path":if i == 0 { "old/renamed.rs".to_owned() } else { format!("src/module/file{i}.rs") }})).collect();
    let results: Vec<CheckResult> = serde_json::from_value(json!([{
        "check_id":"lint", "status":"failed", "duration_ms":1, "findings":findings,
        "files":["docs/reference/old.md", "src/module/file0.rs"]
    }]))
    .unwrap();
    let sample = assess(&policy, &candidate, &results).unwrap();
    assert_eq!(sample.findings.len(), 32);
    assert!(sample
        .findings
        .iter()
        .any(|finding| finding.exception_id.as_deref() == Some("selected")));
    assert!(sample
        .stale_exceptions
        .iter()
        .any(|exception| exception.id == "stale"));
    measure("assessment_32_findings", 100, || {
        black_box(
            assess(
                black_box(&policy),
                black_box(&candidate),
                black_box(&results),
            )
            .unwrap(),
        );
    });
}
