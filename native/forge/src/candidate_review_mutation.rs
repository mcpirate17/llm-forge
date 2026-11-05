//! Held mutation receipt/value observations for the native coordinator.
//! Receipt consistency and compatibility provenance are never execution approval.
#[path = "candidate_mutation_anchor.rs"]
mod anchor;
#[path = "candidate_mutation_context.rs"]
mod context;
#[path = "candidate_mutation_definitions.rs"]
mod definitions;
#[path = "candidate_mutation_git.rs"]
mod git;
#[path = "candidate_mutation_inputs.rs"]
pub(crate) mod inputs;
#[path = "candidate_mutation_native.rs"]
mod native;
#[path = "candidate_mutation_registry.rs"]
pub(crate) mod registry;
#[cfg(test)]
#[path = "candidate_review_mutation_tests.rs"]
mod tests;
#[path = "candidate_mutation_value.rs"]
mod value;
#[path = "candidate_mutation_waivers.rs"]
mod waivers;
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_mutation_external::{Observation, Request},
    candidate_mutation_runner::{Probe, RunnerBundle, RunnerBundleSpec},
    candidate_policy::Policy,
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source},
    candidate_test_selection::{self, inputs::Inputs},
};
use anyhow::{ensure, Context as _, Result};
use git::GitInputs;
use inputs::Capture;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Instant,
};
pub(crate) struct Context<'a> {
    pub source: &'a Source,
    pub snapshot: &'a Snapshot,
    pub candidate: &'a Candidate,
    pub policy: &'a Policy,
    pub today: &'a str,
    pub runner: Option<RunnerBundleSpec<'a>>,
    pub native_runner_pin: Option<(&'a Path, &'a str)>,
    pub stamp: Option<&'a crate::candidate_engine_identity::BuildStamp>,
    pub scratch_exclusions: &'a [PathBuf],
    pub grandfather: Option<&'a crate::candidate_execution_workspace::MutationGrandfather>,
    pub waiver_policy_sha256: Option<&'a str>,
}
/// Retain this provider, its Source/Snapshot/Policy and any applied Stage observation
/// until final disposition. Public output edits fail verify().
pub(crate) struct Evidence<'a> {
    capture: Capture<'a>,
    git: GitInputs<'a>,
    candidate: &'a Candidate,
    policy: &'a Policy,
    candidate_digest: String,
    bundle: Option<RunnerBundle>,
    native: Option<native::Identity>,
    values: Option<value::Values>,
    base_findings: Vec<Finding>,
    date: String,
    requests: Vec<Request>,
    observation: Option<Observation>,
    waivers_authorized: bool,
    output_digest: String,
    pub result: CheckResult,
    pub evidence: Value,
}
impl Evidence<'_> {
    pub fn verify(&self) -> Result<()> {
        self.capture.verify()?;
        self.git.verify()?;
        ensure!(
            candidate_test_selection::digest_json(self.candidate)? == self.candidate_digest,
            "mutation candidate changed"
        );
        ensure!(
            waivers::local_date()? == self.date,
            "mutation waiver date changed; recollect evidence"
        );
        if let Some(bundle) = &self.bundle {
            bundle.verify()?;
        }
        if let Some(native) = &self.native {
            native.verify()?;
        }
        if let Some(observation) = &self.observation {
            observation.verify(self.candidate, self.policy, &self.requests)?;
            observation.verify_probe(
                self.probe()
                    .context("applied invariant observation requires held runner probe")?,
            )?;
        }
        ensure!(
            candidate_test_selection::digest_json(&(&self.result, &self.evidence, &self.requests))?
                == self.output_digest,
            "mutation evidence output changed"
        );
        Ok(())
    }
    pub fn requests(&self) -> &[Request] {
        &self.requests
    }
    pub fn probe(&self) -> Option<&Probe> {
        self.bundle.as_ref().and_then(|b| b.probe.as_ref())
    }
    pub fn apply_external(&mut self, observation: Observation) -> Result<()> {
        self.verify()?;
        ensure!(
            !self.requests.is_empty(),
            "no external-invariant requests were issued"
        );
        observation.verify(self.candidate, self.policy, &self.requests)?;
        observation.verify_probe(
            self.probe()
                .context("external invariant observation requires held runner probe")?,
        )?;
        self.observation = Some(observation);
        self.evidence["external_invariants"] = self
            .observation
            .as_ref()
            .context("invariant observation disappeared")?
            .evidence()
            .clone();
        self.render()?;
        self.verify()
    }
    fn render(&mut self) -> Result<()> {
        let mut findings = self.base_findings.clone();
        if let Some(values) = &self.values {
            findings.extend(values.findings(self.observation.as_ref())?);
        }
        let mut states = if self.waivers_authorized {
            waivers::apply(&mut findings, self.policy, self.candidate, &self.date)?
        } else {
            waivers::value_states(self.policy, self.candidate, &self.date)?
        };
        if !self.waivers_authorized {
            waivers::deny_candidate_waivers(&mut states)?;
        }
        ensure!(findings.len() <= 4096, "mutation findings exceed4096");
        for row in &mut findings {
            candidate_assessment::finalize_finding(row);
        }
        self.result.findings = findings;
        if self.result.skipped_reason.is_none() {
            self.result.status = if self
                .result
                .findings
                .iter()
                .all(|f| f.rule_id == "new-test-value-waived")
            {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            };
        }
        self.result
            .metrics
            .insert("value_waiver_states".into(), states);
        self.result.metrics.insert("value_waiver_base".into(),json!({"commit":self.candidate.waiver_base(),"resolved_by":self.candidate.integration_base_detail}));
        crate::json_canon::encoded_size(&json!([self.result, self.evidence]), 32 * 1024 * 1024)?;
        self.output_digest =
            candidate_test_selection::digest_json(&(&self.result, &self.evidence, &self.requests))?;
        Ok(())
    }
}
fn waiver_authorized(document: &Value, digest: &str, pin: Option<&str>) -> Result<bool> {
    let receipt = list(&document["mutation_waivers"])?;
    let value = list(&document["value_waivers"])?;
    Ok((receipt.is_empty() && value.is_empty()) || pin == Some(digest))
}
pub(crate) fn collect<'a>(context: Context<'a>) -> Result<Evidence<'a>> {
    let started = Instant::now();
    ensure!(
        context.candidate.changes.len() <= 4096,
        "mutation candidate exceeds4096 changes"
    );
    crate::candidate_review_evidence::validate_changes(context.source, context.candidate)?;
    let waivers_authorized = waiver_authorized(
        &context.policy.document,
        &context.policy.digest,
        context.waiver_policy_sha256,
    )?;
    let mut classified = context.candidate.clone();
    context.policy.classify_candidate(&mut classified)?;
    let capture = Capture::new(
        context.source,
        context.snapshot,
        &classified,
        context.policy,
        context.today,
    )?;
    let mut build = Build::new(
        capture,
        GitInputs::new(context.source),
        classified,
        context.grandfather.is_none(),
    );
    build.run(&context, waivers_authorized)?;
    let requests = build
        .values
        .as_ref()
        .map(value::Values::requests)
        .unwrap_or_default();
    let evidence = json!({"schema_version":1,"candidate_tree_oid":context.candidate.tree_oid,"base_tree_oid":context.candidate.base_tree_oid,
        "policy_path":context.policy.path,"policy_sha256":context.policy.digest,"inputs":build.capture.ledger.values().collect::<Vec<_>>(),
        "git_inputs":build.git.evidence,"gated_nodeids":build.gated,"base_definition_errors":build.notes,
        "grandfather_mode":if context.grandfather.is_some(){"operator_pinned"}else{"strict_all_changed_test_definitions"},
        "grandfather_baseline":context.grandfather,
        "waiver_policy_authorized":waivers_authorized,
        "receipt_verification":build.payload,"runner":build.bundle.as_ref().map(|b|&b.evidence),
        "native_runner":build.native.as_ref().map(|value|&value.evidence),
        "original_witnesses":build.values.as_ref().map(value::Values::witnesses),"external_invariant_requests":requests,
        "external_invariants":null,"execution_verified":false,"approval":false,
        "scope":"held receipt consistency and source bindings; external runtime observations recorded separately"});
    let result = CheckResult {
        check_id: "mutation-evidence".into(),
        status: if build.skip.is_some() {
            CheckStatus::Skipped
        } else {
            CheckStatus::Passed
        },
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        findings: Vec::new(),
        files: build.files,
        command: Vec::new(),
        tool_version: Some("forge-mutation-evidence/1".into()),
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: build.skip,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: build.metrics,
    };
    let mut evidence = Evidence {
        capture: build.capture,
        git: build.git,
        candidate: context.candidate,
        policy: context.policy,
        candidate_digest: candidate_test_selection::digest_json(context.candidate)?,
        bundle: build.bundle,
        native: build.native,
        values: build.values,
        base_findings: build.findings,
        date: waivers::local_date()?,
        requests,
        observation: None,
        waivers_authorized,
        output_digest: String::new(),
        result,
        evidence,
    };
    evidence.render()?;
    evidence.verify()?;
    Ok(evidence)
}
struct Build<'a> {
    capture: Capture<'a>,
    git: GitInputs<'a>,
    candidate: Candidate,
    files: Vec<String>,
    gated: Value,
    notes: Vec<Value>,
    findings: Vec<Finding>,
    skip: Option<String>,
    payload: Value,
    bundle: Option<RunnerBundle>,
    native: Option<native::Identity>,
    values: Option<value::Values>,
    metrics: Map<String, Value>,
}
impl<'a> Build<'a> {
    fn new(capture: Capture<'a>, git: GitInputs<'a>, candidate: Candidate, strict: bool) -> Self {
        let files = candidate
            .changes
            .iter()
            .filter(|c| {
                !c.deleted()
                    && (definitions::value_test(c)
                        || strict && c.classes.iter().any(|class| class == "test"))
            })
            .map(|c| c.path.clone())
            .collect();
        Self {
            capture,
            git,
            candidate,
            files,
            gated: json!({}),
            notes: Vec::new(),
            findings: Vec::new(),
            skip: None,
            payload: Value::Null,
            bundle: None,
            native: None,
            values: None,
            metrics: Map::new(),
        }
    }
    fn run(&mut self, ctx: &Context<'a>, waivers_authorized: bool) -> Result<()> {
        if !waivers_authorized {
            self.findings.push(finding("mutation-waiver-policy-untrusted", "candidate mutation waivers require an operator pin matching the exact policy SHA-256".into(), None)?);
        }
        if self.files.is_empty() {
            if self.findings.is_empty() {
                self.skip = Some("no matching candidate test changes".into());
            }
            return Ok(());
        }
        match definitions::grandfather(
            &mut self.capture.inputs,
            &mut self.git,
            &self.candidate,
            ctx.grandfather,
        ) {
            Err(error) => self.findings.push(finding(
                "grandfather-inventory-invalid",
                format!("grandfather inventory fails closed: {error:#}"),
                None,
            )?),
            Ok(grandfathered) => match definitions::gated(
                &mut self.capture.inputs,
                &mut self.git,
                ctx.snapshot,
                &self.candidate,
                &grandfathered,
                ctx.grandfather.is_none(),
            ) {
                Ok((gated, notes)) => {
                    self.gated = gated;
                    self.notes = notes;
                }
                Err(error) => self.findings.push(finding(
                    "new-test-definition-unavailable",
                    format!("new test definitions could not be verified: {error:#}"),
                    None,
                )?),
            },
        }
        let receipt_paths = strings(&decide(
            "receipt_required_paths",
            &json!({"test_paths":self.files,
            "gated_nodeids":if self.findings.is_empty(){&self.gated}else{&Value::Null}}),
        )?)?;
        if receipt_paths.is_empty()
            && self.findings.is_empty()
            && !self.files.iter().any(|p| p.ends_with(".py"))
        {
            self.skip = Some("no supported changed native test definitions detected".into());
            return Ok(());
        }
        if let Err(error) = self.receipts(ctx, &receipt_paths) {
            self.findings.push(finding(
                "mutation-evidence-unavailable",
                format!("mutation evidence could not be verified: {error:#}"),
                None,
            )?);
        }
        Ok(())
    }
    fn receipts(&mut self, ctx: &Context<'a>, paths: &[String]) -> Result<()> {
        let mut states = waivers::receipt_states(&mut self.capture, &self.candidate, ctx.policy)?;
        if !waiver_authorized(
            &ctx.policy.document,
            &ctx.policy.digest,
            ctx.waiver_policy_sha256,
        )? {
            waivers::deny_candidate_waivers(&mut states)?;
        }
        let config = self.capture.config()?;
        let Some(registry) = registry::registry(&mut self.capture, config.as_ref())? else {
            self.findings.push(finding("mutation-registry-missing","candidate snapshot lacks the configured mutation registry; changed tests cannot prove mutation evidence".into(),None)?);
            return Ok(());
        };
        self.metrics
            .insert("registry_path".into(), json!(registry.path));
        let catalog = registry::catalog(&mut self.capture, &registry, paths)?;
        if !paths.is_empty() {
            if let Some(spec) = &ctx.runner {
                self.bundle = Some(RunnerBundle::capture(
                    RunnerBundleSpec {
                        manifest: spec.manifest,
                        sha256: spec.sha256,
                    },
                    ctx.source,
                    ctx.snapshot,
                    ctx.scratch_exclusions,
                )?);
            }
        }
        let runtime = self
            .bundle
            .as_ref()
            .map(|b| json!({"runner":b.runner,"binding_sha256":b.bindings,"lineage":b.lineage}));
        let native_fingerprint = if catalog.has_native_receipts {
            let fingerprint = self.capture.native_source_fingerprint()?;
            if fingerprint.is_none() {
                self.native = native::Identity::capture(ctx, &mut self.capture)?;
            }
            fingerprint
        } else {
            None
        };
        let supporting_capture = crate::mutation_cargo_supporting::required_for(
            &catalog.contracts,
            &catalog.receipts,
            || Ok(native_fingerprint.clone()),
            self.native.as_ref().map(native::Identity::identity),
        )
        .and_then(|required| {
            required
                .map(|paths| self.capture.supporting_inventory(&paths))
                .transpose()
        });
        let supporting_capture = supporting_capture.map_err(|error| format!("{error:#}"));
        let supporting = supporting_capture
            .as_ref()
            .map(Option::as_ref)
            .map_err(String::as_str);
        let verified=conductor_native::mutation_evidence::verify_captured_native_with_supporting(&json!({"candidate_paths":paths,"campaigns":catalog.contracts,
            "runtime":runtime,"broken_campaigns":catalog.broken,"malformed_receipts":catalog.malformed}),&catalog.receipts,
            |path,bytes,contract|anchor::prove(&mut self.git,path,bytes,contract,ctx.grandfather).map_err(|e|format!("{e:#}")),
            native_fingerprint.as_deref(), self.native.as_ref().map(native::Identity::identity), supporting).map_err(anyhow::Error::msg)?;
        self.payload = verified.result;
        let waived: Vec<_> = list(&states)?
            .iter()
            .filter(|s| s["active"] == true)
            .map(|s| s["path"].clone())
            .collect();
        self.findings.extend(rows(&decide(
            "receipt_findings",
            &json!({"payload":self.payload,"waived":waived}),
        )?)?);
        self.metrics = decide(
            "mutation_metrics",
            &json!({"payload":self.payload,"new_nodeids":self.gated,"waiver_states":states}),
        )?
        .as_object()
        .context("mutation metrics must be an object")?
        .clone();
        self.metrics
            .insert("registry_path".into(), json!(registry.path));
        self.metrics.insert(
            "receipt_exempt_tests".into(),
            json!(self
                .files
                .iter()
                .filter(|p| !paths.contains(p))
                .collect::<Vec<_>>()),
        );
        self.values = Some(value::Values::new(
            &self.payload,
            &self.gated,
            verified.attempts,
            &catalog.raw_manifests,
            config.as_ref(),
        )?);
        Ok(())
    }
}
fn decide(operation: &str, payload: &Value) -> Result<Value> {
    crate::json_canon::encoded_size(payload, 32 * 1024 * 1024)?;
    conductor_native::candidate_verification::decide(operation, payload).map_err(anyhow::Error::msg)
}
fn list(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .context("mutation evidence field must be an array")
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("mutation field {key} must be a string"))
}
fn strings(value: &Value) -> Result<Vec<String>> {
    list(value)?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .context("mutation path/nodeid must be a string")
        })
        .collect()
}
fn rows(value: &Value) -> Result<Vec<Finding>> {
    serde_json::from_value(value.clone()).context("invalid native mutation findings")
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn finding(rule: &str, message: String, path: Option<&str>) -> Result<Finding> {
    serde_json::from_value(json!({"check_id":"mutation-evidence","rule_id":rule,"severity":"critical","message":message,"path":path})).map_err(Into::into)
}
