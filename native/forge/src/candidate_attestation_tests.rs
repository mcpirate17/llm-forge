use super::*;
use git2::{Repository, Signature};
use std::{fs, os::unix::fs::symlink, path::PathBuf};
pub(super) struct Fixture {
    pub(super) source: Source,
    pub(super) candidate: Candidate,
    policy: Policy,
    _directory: tempfile::TempDir,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(directory.path()).unwrap();
        let mut index = repository.index().unwrap();
        let tree = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repository.find_tree(tree).unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let first = repository
            .commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
            .unwrap();
        let first = repository.find_commit(first).unwrap();
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "target",
                &tree,
                &[&first],
            )
            .unwrap();
        let source = Source::open(directory.path()).unwrap();
        let candidate = source.resolve(Kind::Commit, "HEAD", None).unwrap();
        let policy = Policy {
            path: "policy.toml".into(),
            digest: "f".repeat(64),
            document: json!({}),
        };
        Self {
            source,
            candidate,
            policy,
            _directory: directory,
        }
    }
    fn receipt(&self, tree: &str, policy: &str, decision: &str) -> Vec<u8> {
        let value = json!({"schema_version":1,"surface":"pre-commit","profile":"fast","candidate":{"tree_oid":tree},"policy":{"sha256":policy},"decision":decision});
        let sealed = candidate_receipt::seal(&serde_json::to_vec(&value).unwrap()).unwrap();
        serde_json::to_vec(sealed.payload()).unwrap()
    }
    fn path(&self) -> PathBuf {
        self.source.common_dir().join(format!(
            "governance/receipts/pre-commit/{}-fast.json",
            self.candidate.tree_oid
        ))
    }
    fn save(&self, bytes: &[u8]) {
        let path = self.path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn collect(&self) -> Value {
        collect_post_commit(&self.source, &self.candidate, &self.policy).unwrap()
    }
}
fn recovery(value: &Value) {
    let findings = value["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["check_id"], "attestation");
    assert_eq!(findings[0]["rule_id"], "precommit-bypass-recovered");
    assert_eq!(findings[0]["severity"], "medium");
    assert_eq!(value["metrics"]["precommit_receipt"], "missing-or-invalid");
    assert_eq!(value["metrics"]["approval"], false);
    assert_eq!(value["metrics"]["execution_verified"], false);
}

#[test]
fn matching_recorded_pass_is_observed_without_claiming_execution_or_approval() {
    let f = Fixture::new();
    f.save(&f.receipt(&f.candidate.tree_oid, &f.policy.digest, "pass"));
    let index = fs::read(f.source.common_dir().join("index")).unwrap();
    let head = fs::read(f.source.common_dir().join("HEAD")).unwrap();
    let value = f.collect();
    assert!(value["findings"].as_array().unwrap().is_empty());
    assert_eq!(
        value["metrics"]["precommit_receipt"],
        "matching-recorded-pass"
    );
    assert_eq!(value["metrics"]["candidate_tree_oid"], f.candidate.tree_oid);
    assert_eq!(value["metrics"]["policy_sha256"], f.policy.digest);
    assert_eq!(
        value["metrics"]["receipt_digest"].as_str().unwrap().len(),
        64
    );
    for field in ["approval", "execution_verified", "review_complete"] {
        assert_eq!(value["metrics"][field], false);
    }
    assert_eq!(
        fs::read(f.source.common_dir().join("index")).unwrap(),
        index
    );
    assert_eq!(fs::read(f.source.common_dir().join("HEAD")).unwrap(), head);
}
#[test]
fn missing_failed_and_differently_bound_receipts_require_recovery() {
    let f = Fixture::new();
    recovery(&f.collect());
    for (tree, policy, decision) in [
        ("a".repeat(40), f.policy.digest.clone(), "pass"),
        (f.candidate.tree_oid.clone(), "a".repeat(64), "pass"),
        (
            f.candidate.tree_oid.clone(),
            f.policy.digest.clone(),
            "fail",
        ),
    ] {
        f.save(&f.receipt(&tree, &policy, decision));
        recovery(&f.collect());
    }
}
#[test]
fn receipt_checksum_verifier_is_used_instead_of_trusting_recorded_pass_fields() {
    let f = Fixture::new();
    let mut value: Value =
        serde_json::from_slice(&f.receipt(&f.candidate.tree_oid, &f.policy.digest, "pass"))
            .unwrap();
    value["unexpected_tamper"] = json!(true);
    f.save(&serde_json::to_vec(&value).unwrap());
    let observed = f.collect();
    recovery(&observed);
    assert!(observed["metrics"]["detail"]
        .as_str()
        .unwrap()
        .contains("digest mismatch"));
}
#[test]
fn capability_read_does_not_follow_receipt_symlinks_outside_common_git_directory() {
    let f = Fixture::new();
    let outside = f.source.root().join("external-receipt.json");
    fs::write(
        &outside,
        f.receipt(&f.candidate.tree_oid, &f.policy.digest, "pass"),
    )
    .unwrap();
    let path = f.path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(outside, &path).unwrap();
    recovery(&f.collect());
}
#[test]
fn invalid_resolved_source_or_policy_context_is_an_error_not_a_recovery_observation() {
    let f = Fixture::new();
    let mut candidate = f.candidate.clone();
    candidate.tree_oid = "a".repeat(40);
    assert!(collect_post_commit(&f.source, &candidate, &f.policy).is_err());
    candidate = f.candidate.clone();
    candidate.commit_oid = None;
    assert!(collect_post_commit(&f.source, &candidate, &f.policy).is_err());
    candidate = f.candidate.clone();
    candidate.kind = Kind::Index;
    assert!(collect_post_commit(&f.source, &candidate, &f.policy).is_err());
    let invalid = Policy {
        path: f.policy.path.clone(),
        digest: "".into(),
        document: json!({}),
    };
    assert!(collect_post_commit(&f.source, &f.candidate, &invalid).is_err());
}

#[test]
fn checksum_consistent_receipt_from_another_surface_or_profile_does_not_match() {
    let f = Fixture::new();
    for (surface, profile) in [("ci", "fast"), ("pre-commit", "full")] {
        let value = json!({"schema_version":1,"surface":surface,"profile":profile,"candidate":{"tree_oid":f.candidate.tree_oid},
            "policy":{"sha256":f.policy.digest},"decision":"pass"});
        let receipt = candidate_receipt::seal(&serde_json::to_vec(&value).unwrap()).unwrap();
        f.save(&serde_json::to_vec(receipt.payload()).unwrap());
        recovery(&f.collect());
    }
}
