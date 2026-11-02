//! Inspect immutable commit objects without invoking Git or trusting trailers as authorization.
use super::*;
use git2::{ObjectType, Oid, Sort};
use std::collections::BTreeMap;

const AGENT_REQUIRED_FROM: i64 = 1_788_706_800;
const MAX_COMMITS: usize = 4096;
const MAX_COMMIT_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

fn agent(message: &str) -> bool {
    message.lines().any(|line| {
        let Some(value) = line.strip_prefix("Agent:") else {
            return false;
        };
        let value = value.trim_matches([' ', '\t', '\r']);
        value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    })
}
fn trailers(message: &str) -> BTreeMap<&str, &str> {
    message
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            matches!(
                key,
                "Governance-Tree" | "Governance-Policy" | "Governance-Receipt"
            )
            .then_some((key, value.trim()))
        })
        .collect()
}

pub(crate) fn collect(source: &Source, candidate: &Candidate) -> Result<Value> {
    let tip = candidate
        .commit_oid
        .as_deref()
        .context("CI review requires a commit")?;
    ensure!(
        source.commit_tree(tip)? == candidate.tree_oid,
        "CI candidate tree mismatch"
    );
    let repo = source.repository();
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
    walk.push(Oid::from_str(tip)?)?;
    if let Some(base) = &candidate.base_commit_oid {
        ensure!(
            source.commit_tree(base)? == candidate.base_tree_oid,
            "CI base tree mismatch"
        );
        walk.hide(Oid::from_str(base)?)?;
    }
    let odb = repo.odb()?;
    let (mut commits, mut missing, mut mismatched, mut unattributed) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut total = 0usize;
    for id in walk {
        ensure!(
            commits.len() < MAX_COMMITS,
            "CI attestation exceeds 4096 commits"
        );
        let id = id?;
        let (bytes, kind) = odb.read_header(id)?;
        ensure!(
            kind == ObjectType::Commit && bytes <= MAX_COMMIT_BYTES,
            "CI commit object exceeds 1 MiB or has wrong type"
        );
        total = total
            .checked_add(bytes)
            .context("CI commit size overflow")?;
        ensure!(total <= MAX_TOTAL_BYTES, "CI commit objects exceed 16 MiB");
        let commit = repo.find_commit(id)?;
        let message = std::str::from_utf8(commit.message_bytes())
            .context("CI commit message must be UTF-8")?;
        let recorded = trailers(message);
        let id = id.to_string();
        if recorded.is_empty() {
            missing.push(id.clone());
        } else if recorded.get("Governance-Tree").copied()
            != Some(commit.tree_id().to_string().as_str())
        {
            mismatched.push(id.clone());
        }
        if !agent(message) && commit.committer().when().seconds() >= AGENT_REQUIRED_FROM {
            unattributed.push(id.clone());
        }
        commits.push(id);
    }
    let mut findings = Vec::new();
    if !unattributed.is_empty() {
        findings.push(
            Finding::new(
                "attestation",
                "commit-agent-unattributed",
                "high",
                "candidate commits lack an Agent trailer naming their originating session",
            )
            .evidence(json!({"commits":unattributed})),
        );
    }
    if !missing.is_empty() || !mismatched.is_empty() {
        findings.push(Finding::new("attestation", "commit-hook-bypass-detected", "medium",
            "candidate commits lack matching local tree attestations; fresh review evidence is required")
            .evidence(json!({"missing":missing,"mismatched":mismatched})));
    }
    Ok(
        json!({"findings":findings,"metrics":{"surface":"ci","commits":commits,
        "commits_examined":commits.len(),"missing_attestations":missing,
        "mismatched_attestations":mismatched,"unattributed_commits":unattributed,
        "evidence_scope":"immutable-commit-trailer-observation","approval":false}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_trailer_requires_complete_line_and_valid_identity() {
        for value in ["Agent: forge-1", "subject\n\nAgent:\tforge.2_3\t\r\n"] {
            assert!(agent(value));
        }
        for value in [
            "Agent: ",
            "Agent: -forge",
            "Agent: forge extra",
            " Agent: forge",
            "Agent: forgé",
            "Agent: forge\u{1b}",
        ] {
            assert!(!agent(value), "{value:?}");
        }
        assert_eq!(
            trailers("Governance-Tree: old\nGovernance-Tree: new")["Governance-Tree"],
            "new"
        );
    }
    #[test]
    fn immutable_range_checks_each_tree_and_attribution_date() {
        let f = super::super::tests::Fixture::new();
        let repo = f.source.repository();
        let parent = repo
            .find_commit(Oid::from_str(f.candidate.commit_oid.as_ref().unwrap()).unwrap())
            .unwrap();
        let tree = parent.tree().unwrap();
        let signature = git2::Signature::new(
            "fixture",
            "fixture@example.invalid",
            &git2::Time::new(AGENT_REQUIRED_FROM - 1, 0),
        )
        .unwrap();
        let old = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "old commit",
                &tree,
                &[&parent],
            )
            .unwrap();
        let old = repo.find_commit(old).unwrap();
        let current = git2::Signature::new(
            "fixture",
            "fixture@example.invalid",
            &git2::Time::new(AGENT_REQUIRED_FROM, 0),
        )
        .unwrap();
        let good = repo
            .commit(
                Some("HEAD"),
                &current,
                &current,
                &format!("good\n\nAgent: unit\nGovernance-Tree: {}", tree.id()),
                &tree,
                &[&old],
            )
            .unwrap();
        let good = repo.find_commit(good).unwrap();
        let bad = repo
            .commit(
                Some("HEAD"),
                &current,
                &current,
                "bad\n\nGovernance-Tree: wrong",
                &tree,
                &[&good],
            )
            .unwrap();
        let candidate = f
            .source
            .resolve(Kind::Range, "HEAD", Some(&parent.id().to_string()))
            .unwrap();
        let value = collect(&f.source, &candidate).unwrap();
        assert_eq!(value["metrics"]["commits_examined"], 3);
        assert_eq!(
            value["metrics"]["missing_attestations"],
            json!([old.id().to_string()])
        );
        assert_eq!(
            value["metrics"]["mismatched_attestations"],
            json!([bad.to_string()])
        );
        assert_eq!(
            value["metrics"]["unattributed_commits"],
            json!([bad.to_string()])
        );
        assert_eq!(value["findings"].as_array().unwrap().len(), 2);
    }
}
