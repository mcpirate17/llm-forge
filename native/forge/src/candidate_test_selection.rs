//! Captured test-selection evidence for a future native review coordinator.
//! This performs no test execution and does not claim graph or review coverage.
#[path = "candidate_test_inputs.rs"]
pub(crate) mod inputs;
#[path = "candidate_test_inventory.rs"]
mod inventory;
#[cfg(test)]
#[path = "candidate_test_selection_tests.rs"]
mod tests;
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_policy::Policy,
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source},
};
use anyhow::{ensure, Context, Result};
use inputs::Inputs;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, io::Write, path::Path, sync::OnceLock, time::Instant};

/// Optional operator-owned, SHA-256-pinned runtime registry when the candidate
/// itself does not carry one. The external file is held and reverified.
pub struct ExternalRegistry<'a> {
    pub path: &'a Path,
    pub sha256: &'a str,
}

/// Owns captured input bindings until the coordinator completes its use.
pub struct Selection<'a> {
    inputs: Inputs<'a>,
    candidate: &'a Candidate,
    candidate_digest: String,
    output_digest: String,
    pub result: CheckResult,
    pub python_tests: Vec<String>,
    pub native_tests: Map<String, Value>,
    pub contract_targets: Vec<String>,
    pub contract_test_paths: Vec<String>,
    pub evidence: Value,
}
impl Selection<'_> {
    fn outputs_digest(&self) -> Result<String> {
        selection_output_digest(
            &self.result,
            &self.python_tests,
            &self.native_tests,
            &self.contract_targets,
            &self.contract_test_paths,
            &self.evidence,
        )
    }
    pub(crate) fn selected_evidence_paths(
        &self,
        source: &Source,
        snapshot: &Snapshot,
        candidate: &Candidate,
        policy: &Policy,
    ) -> Result<Vec<String>> {
        self.verify()?;
        self.inputs.check_context(source, snapshot, policy)?;
        ensure!(
            digest_candidate(candidate)? == self.candidate_digest,
            "test selection candidate differs from evidence candidate"
        );
        let mut paths: BTreeSet<_> = self
            .python_tests
            .iter()
            .chain(&self.contract_test_paths)
            .cloned()
            .collect();
        for row in self.native_tests.values() {
            paths.extend(strings(row)?);
        }
        ensure!(paths.len() <= 4096, "selected evidence exceeds 4096 paths");
        for path in &paths {
            crate::candidate_source::safe_path(path.as_bytes())?;
        }
        Ok(paths.into_iter().collect())
    }
    pub fn verify(&self) -> Result<()> {
        self.inputs.verify()?;
        ensure!(
            self.outputs_digest()? == self.output_digest,
            "test selection output changed"
        );
        ensure!(
            digest_candidate(self.candidate)? == self.candidate_digest,
            "candidate selection input changed"
        );
        Ok(())
    }
}

/// Caller retains the owned immutable snapshot and policy through execution.
/// Collection errors must be converted into blocking test-evidence errors.
pub fn collect<'a>(
    source: &Source,
    snapshot: &'a Snapshot,
    candidate: &'a Candidate,
    policy: &'a Policy,
    today: &'a str,
    external: Option<ExternalRegistry<'_>>,
) -> Result<Selection<'a>> {
    let started = Instant::now();
    ensure!(
        candidate.changes.len() <= 4096,
        "test selection exceeds 4096 candidate changes"
    );
    changes(candidate)?;
    let supplied_candidate_sha256 = digest_candidate(candidate)?;
    let supplied_candidate = candidate;
    let mut classified = candidate.clone();
    policy.classify_candidate(&mut classified)?;
    let candidate = &classified;
    let changes = changes(candidate)?;
    let plan = conductor_native::candidate_verification::decide(
        "selection_plan",
        &json!({"changes":changes}),
    )
    .map_err(anyhow::Error::msg)?;
    let sources = strings(&plan["sources"])?;
    let graph_needed = !strings(&plan["graph_paths"])?.is_empty();
    let mut inputs = Inputs::new(source, snapshot, candidate, policy, today)?;
    let graph_tests = inventory::python_tests(&inputs, graph_needed)?;
    let native_tests = inventory::rust_tests(&mut inputs, &sources)?;
    let changed_paths = candidate
        .changes
        .iter()
        .flat_map(|change| std::iter::once(change.path.clone()).chain(change.old_path.clone()))
        .collect::<Vec<_>>();
    let external_ref = external.as_ref().map(|item| (item.path, item.sha256));
    let contract = inventory::contracts(&mut inputs, &changed_paths, external_ref)?;
    let evidence_tests = evidence_paths(&graph_tests, &plan, &native_tests, &contract.paths)?;
    for path in &evidence_tests {
        inputs.read(path)?;
    }
    let marker = if plan["high_risk"] == true {
        property_marker(&mut inputs, &evidence_tests)?
    } else {
        None
    };
    let decision = decide(
        &plan,
        &graph_tests,
        &native_tests,
        &contract,
        marker.is_some(),
        graph_needed,
    )?;
    let python_tests = strings(&decision["tests"])?;
    let uncovered_native = uncovered_native(candidate, &native_tests, &contract);
    let (result, blocked) = result(
        &decision,
        &contract,
        marker.as_deref(),
        plan["high_risk"] == true,
        &uncovered_native,
        started,
    )?;
    let candidate_sha256 = digest_candidate(candidate)?;
    let selection_sha256 = digest_json(&SelectionDigest {
        python_tests: &python_tests,
        native_tests: &native_tests,
        contract_targets: &contract.targets,
        contract_test_paths: &contract.paths,
    })?;
    let evidence = json!({"schema_version":1,"candidate_tree":candidate.tree_oid,"supplied_candidate_sha256":supplied_candidate_sha256,"classified_candidate_sha256":candidate_sha256,"selection_sha256":selection_sha256,"policy_path":policy.path,"policy_sha256":policy.digest,"graph":decision["graph"],"property_marker_path":marker,"property_marker_scope":"legacy_text_heuristic_not_execution_proof","registry_scope": if inputs.entries().contains_key("native/conductor-native/src/python_contract_targets.tsv") {"candidate_tree"} else if external.is_some() {"operator_pinned"} else {"absent"},"registry_git_oid":inputs.entries().get("native/conductor-native/src/python_contract_targets.tsv").map(|entry|&entry.oid),"registry_extension_git_oid":inputs.entries().get("native/conductor-native/src/python_contract_targets_extra.tsv").map(|entry|&entry.oid),"external_registry_sha256":external.as_ref().map(|item|item.sha256),"test_execution_verified":false,"review_complete":false,"blocked_unavailable_evidence":blocked});
    inputs.verify()?;
    let output_digest = selection_output_digest(
        &result,
        &python_tests,
        &native_tests,
        &contract.targets,
        &contract.paths,
        &evidence,
    )?;
    Ok(Selection {
        inputs,
        candidate: supplied_candidate,
        candidate_digest: supplied_candidate_sha256,
        output_digest,
        result,
        python_tests,
        native_tests,
        contract_targets: contract.targets,
        contract_test_paths: contract.paths,
        evidence,
    })
}
fn decide(
    plan: &Value,
    graph_tests: &[String],
    native_tests: &Map<String, Value>,
    contract: &inventory::Contracts,
    marker: bool,
    graph_needed: bool,
) -> Result<Value> {
    let graph = if graph_needed {
        json!({"status":"unverified", "complete":false, "scope":"candidate_tracked_conventional_test_inventory", "fallback_reasons":["graph freshness and coverage not established for this captured candidate"], "selected_edges":0})
    } else {
        json!({"status":"not-required", "complete":false, "scope":"no_graph_trigger", "selected_edges":0})
    };
    let mut request = plan.clone();
    let object = request
        .as_object_mut()
        .context("native selection plan was not an object")?;
    object.insert("graph_tests".into(), json!(graph_tests));
    object.insert("native_tests".into(), Value::Object(native_tests.clone()));
    object.insert("contract_sources".into(), json!(contract.sources));
    object.insert("contract_targets".into(), json!(contract.targets));
    object.insert("contract_test_paths".into(), json!(contract.paths));
    object.insert("graph".into(), graph);
    object.insert("graph_error".into(), Value::Null);
    object.insert("property_evidence".into(), json!(marker));
    conductor_native::candidate_verification::decide("selection_decide", &request)
        .map_err(anyhow::Error::msg)
}
fn changes(candidate: &Candidate) -> Result<Vec<Value>> {
    let mut changes = Vec::with_capacity(candidate.changes.len());
    let mut path_bytes = 0usize;
    for change in &candidate.changes {
        crate::candidate_source::safe_path(change.path.as_bytes())?;
        if let Some(old) = &change.old_path {
            crate::candidate_source::safe_path(old.as_bytes())?;
        }
        path_bytes = path_bytes
            .checked_add(change.path.len())
            .and_then(|n| n.checked_add(change.old_path.as_ref().map_or(0, String::len)))
            .context("candidate selection path bytes overflow")?;
        ensure!(
            path_bytes <= 8 * 1024 * 1024 && change.risk.len() <= 256,
            "candidate selection fields exceed bounds"
        );
        ensure!(
            change.classes.len() <= 64 && change.classes.iter().all(|s| s.len() <= 256),
            "candidate classes exceed selection limits"
        );
        changes.push(json!({"path":change.path,"classes":change.classes,"deleted":change.deleted(),"risk":change.risk}));
    }
    Ok(changes)
}
fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("native selection returned non-array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("native selection returned non-string")
        })
        .collect()
}
fn digest_candidate(candidate: &Candidate) -> Result<String> {
    digest_json(candidate)
}
#[derive(serde::Serialize)]
struct SelectionDigest<'a> {
    python_tests: &'a [String],
    native_tests: &'a Map<String, Value>,
    contract_targets: &'a [String],
    contract_test_paths: &'a [String],
}
pub(crate) fn digest_json(value: &impl serde::Serialize) -> Result<String> {
    struct Sink {
        hash: Sha256,
        bytes: usize,
    }
    impl Write for Sink {
        fn write(&mut self, raw: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(raw.len())
                .filter(|n| *n <= 32 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("candidate selection input exceeds 32 MiB"))?;
            self.hash.update(raw);
            Ok(raw.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut sink, value)?;
    Ok(format!("{:x}", sink.hash.finalize()))
}
fn evidence_paths(
    graph_tests: &[String],
    plan: &Value,
    native: &Map<String, Value>,
    contracts: &[String],
) -> Result<Vec<String>> {
    let mut paths: BTreeSet<String> = graph_tests.iter().chain(contracts).cloned().collect();
    paths.extend(strings(&plan["changed_tests"])?);
    for value in native.values() {
        paths.extend(strings(value)?);
    }
    ensure!(
        paths.len() <= 4096,
        "test selection evidence exceeds 4096 paths"
    );
    Ok(paths.into_iter().collect())
}
fn property_marker(inputs: &mut Inputs<'_>, paths: &[String]) -> Result<Option<String>> {
    static PROPERTY: OnceLock<Regex> = OnceLock::new();
    let regex = PROPERTY.get_or_init(|| Regex::new(r"hypothesis|@(?:pytest\.)?mark\.parametrize|@pytest\.mark\.(?:property|invariant)|def test_[^\n]*(?:property|invariant|boundary|roundtrip|adversarial)").expect("static property marker regex"));
    for path in paths {
        if regex.is_match(inputs.read(path)?) {
            return Ok(Some(path.clone()));
        }
    }
    Ok(None)
}
fn result(
    decision: &Value,
    contract: &inventory::Contracts,
    marker: Option<&str>,
    high_risk: bool,
    uncovered_native: &[String],
    started: Instant,
) -> Result<(CheckResult, bool)> {
    let mut findings = decision["findings"]
        .as_array()
        .context("selection findings missing")?
        .iter()
        .map(|value| serde_json::from_value::<Finding>(value.clone()).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>>>()?;
    let mut blocked = false;
    if contract.absent_relevant {
        blocked = true;
        findings.push(serde_json::from_value(json!({"check_id":"test-evidence","rule_id":"contract-registry-unavailable","severity":"critical","message":"potentially mapped changes require the captured or an operator-pinned contract registry"}))?);
    }
    if high_risk && marker.is_none() {
        blocked = true;
        findings.push(serde_json::from_value(json!({"check_id":"test-evidence","rule_id":"mutation-evidence-unverified","severity":"critical","message":"no captured property-style marker was found and mutation evidence has not been authenticated"}))?);
    }
    if !uncovered_native.is_empty() {
        blocked = true;
        findings.push(serde_json::from_value(json!({"check_id":"test-evidence","rule_id":"native-test-targets-unverified","severity":"critical","message":"native source lacks a captured crate or contract test mapping while graph coverage is unverified","evidence":{"paths":uncovered_native}}))?);
    }
    for finding in &mut findings {
        candidate_assessment::finalize_finding(finding);
    }
    let files = strings(&decision["tests"])?
        .into_iter()
        .chain(contract.paths.clone())
        .collect();
    let metrics = decision["graph"]
        .as_object()
        .cloned()
        .context("selection graph missing")?;
    Ok((
        CheckResult {
            check_id: "test-evidence".into(),
            status: if blocked {
                CheckStatus::Error
            } else if findings.is_empty() {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            findings,
            files,
            command: Vec::new(),
            tool_version: None,
            cache_key: None,
            cache_hit: false,
            exit_code: None,
            skipped_reason: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            metrics,
        },
        blocked,
    ))
}

fn selection_output_digest(
    result: &CheckResult,
    python_tests: &[String],
    native_tests: &Map<String, Value>,
    contract_targets: &[String],
    contract_test_paths: &[String],
    evidence: &Value,
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Outputs<'a> {
        result: &'a CheckResult,
        python_tests: &'a [String],
        native_tests: &'a Map<String, Value>,
        contract_targets: &'a [String],
        contract_test_paths: &'a [String],
        evidence: &'a Value,
    }
    digest_json(&Outputs {
        result,
        python_tests,
        native_tests,
        contract_targets,
        contract_test_paths,
        evidence,
    })
}

fn uncovered_native(
    candidate: &Candidate,
    native_tests: &Map<String, Value>,
    contract: &inventory::Contracts,
) -> Vec<String> {
    candidate
        .changes
        .iter()
        .filter(|change| {
            !change.deleted()
                && change.classes.iter().any(|class| class == "native")
                && !change.classes.iter().any(|class| class == "test")
                && !native_tests.contains_key(&change.path)
                && !contract.sources.contains(&change.path)
        })
        .map(|change| change.path.clone())
        .collect::<Vec<_>>()
}
