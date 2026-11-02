//! Value admission over retained originals and opaque owned execution observations.
use super::*;
use crate::candidate_mutation_external::{Observation, Request};
use conductor_native::mutation_evidence::CapturedAttempt;
pub(super) struct Values {
    plan: Value,
    contexts: BTreeMap<String, Vec<String>>,
    attempts: Vec<CapturedAttempt>,
    declared: BTreeMap<String, Request>,
    invalid: Vec<Finding>,
}
impl Values {
    pub fn new(
        payload: &Value,
        gated: &Value,
        attempts: Vec<CapturedAttempt>,
        manifests: &BTreeMap<String, Value>,
        config: Option<&conductor_native::project_config::ProjectConfig>,
    ) -> Result<Self> {
        let plan = decide(
            "value_admission_plan",
            &json!({"payload":payload,"new_nodeids":gated}),
        )?;
        let contexts = context::declarations(config, gated)?;
        let nodes: usize = gated
            .as_object()
            .context("gated nodeids must be an object")?
            .values()
            .map(|v| v.as_array().map_or(0, Vec::len))
            .sum();
        let sources: usize = contexts.values().map(Vec::len).sum();
        ensure!(
            nodes <= 4096 && sources <= 4096,
            "value admission inventory exceeds4096"
        );
        let factor = nodes.saturating_add(sources).saturating_add(1);
        let mut work = 0usize;
        for attempt in &attempts {
            work = work.saturating_add(
                crate::json_canon::encoded_size(&attempt.expanded, 32 * 1024 * 1024)?
                    .saturating_mul(factor),
            );
            ensure!(
                work <= 32 * 1024 * 1024,
                "value admission byte amplification exceeds32MiB"
            );
        }
        let mut this = Self {
            plan,
            contexts,
            attempts,
            declared: BTreeMap::new(),
            invalid: Vec::new(),
        };
        this.declarations(manifests)?;
        Ok(this)
    }
    fn declarations(&mut self, manifests: &BTreeMap<String, Value>) -> Result<()> {
        for step in list(&self.plan["steps"])? {
            let Some(task) = step.get("task") else {
                continue;
            };
            let matching: Vec<_> = manifests
                .values()
                .filter(|m| m["campaign_id"] == task["evidence"]["campaign_id"])
                .collect();
            ensure!(
                matching.len() <= 1,
                "ambiguous external-invariant campaign identity"
            );
            let Some(manifest) = matching.first() else {
                continue;
            };
            let Some(rows) = manifest.get("external_invariants") else {
                continue;
            };
            let nodes = strings(&task["nodeids"])?;
            for row in list(rows)? {
                let Some(node) = row["nodeid"]
                    .as_str()
                    .filter(|n| nodes.iter().any(|v| v == n))
                else {
                    continue;
                };
                let justification = row["justification"].as_str().unwrap_or("");
                let pin = row["pinned"]["torch"].as_str().unwrap_or("");
                if justification.trim().is_empty() || pin.trim().is_empty() {
                    self.invalid.push(external_failure(
                        node,
                        "torch pin and non-empty justification are required",
                    )?);
                    continue;
                }
                ensure!(
                    node.len() <= 4096 && pin.len() <= 4096 && justification.len() <= 4096,
                    "external-invariant declaration exceeds bounds"
                );
                ensure!(
                    !self.declared.contains_key(node),
                    "duplicate external-invariant declaration"
                );
                self.declared.insert(
                    node.into(),
                    Request {
                        nodeid: node.into(),
                        pinned_torch: pin.into(),
                        justification: justification.into(),
                    },
                );
                ensure!(
                    self.declared.len() <= 128,
                    "external-invariant requests exceed128"
                );
            }
        }
        Ok(())
    }
    pub fn requests(&self) -> Vec<Request> {
        self.declared.values().cloned().collect()
    }
    pub fn findings(&self, observation: Option<&Observation>) -> Result<Vec<Finding>> {
        let mut findings = rows(&self.plan["prefix_findings"])?;
        findings.extend(self.invalid.iter().cloned());
        let mut admitted = BTreeSet::new();
        if let Some(observation) = observation {
            let outcomes = observation.outcomes();
            ensure!(
                outcomes.len() == self.declared.len(),
                "invariant outcome inventory differs"
            );
            let mut seen = BTreeSet::new();
            for row in outcomes {
                ensure!(
                    self.declared.contains_key(&row.nodeid) && seen.insert(&row.nodeid),
                    "invariant outcome identity differs"
                );
                if row.admitted {
                    admitted.insert(row.nodeid.clone());
                } else {
                    findings.push(external_failure(&row.nodeid, &row.reason)?);
                }
            }
        } else {
            for node in self.declared.keys() {
                findings.push(external_failure(
                    node,
                    "owned Stage observation is required",
                )?);
            }
        }
        for step in list(&self.plan["steps"])? {
            if let Some(row) = step.get("finding") {
                findings.push(serde_json::from_value(row.clone())?);
                continue;
            }
            self.task(&step["task"], &admitted, &mut findings)?;
        }
        ensure!(findings.len() <= 4096, "mutation value findings exceed4096");
        Ok(findings)
    }
    fn task(
        &self,
        task: &Value,
        admitted: &BTreeSet<String>,
        findings: &mut Vec<Finding>,
    ) -> Result<()> {
        let path = string(task, "path")?;
        let nodes = strings(&task["nodeids"])?;
        let attempts: Vec<_> = self
            .attempts
            .iter()
            .filter(|a| {
                a.receipt == task["receipt"].as_str().unwrap_or("")
                    && Some(a.campaign_id.as_str()) == task["evidence"]["campaign_id"].as_str()
            })
            .collect();
        ensure!(
            attempts.len() <= 1,
            "selected receipt maps to ambiguous campaign contracts"
        );
        let Some(attempt) = attempts.first() else {
            findings.push(finding(
                "test-value-receipt-unavailable",
                format!("{path}: selected test-value receipt was not captured"),
                Some(path),
            )?);
            return Ok(());
        };
        let mut errors = Vec::new();
        for node in nodes.iter().filter(|node| !admitted.contains(*node)) {
            let incomplete = context::complete(&attempt.expanded);
            if !attempt.valid || !incomplete.is_empty() {
                errors.push(format!(
                    "new test {} has incomplete automatic PASS evidence: {}",
                    context::repr(node),
                    if incomplete.is_empty() {
                        "native receipt validation refused".into()
                    } else {
                        incomplete.join("; ")
                    }
                ));
                continue;
            }
            errors.extend(if let Some(sources) = self.contexts.get(node) {
                context::errors(node, sources, &self.attempts)?
            } else {
                conductor_native::mutation_value_admission::admission_errors(
                    &attempt.expanded["test_value"],
                    std::slice::from_ref(node),
                )
                .map_err(anyhow::Error::msg)?
            });
        }
        findings.extend(rows(&decide(
            "value_admission_findings",
            &json!({"path":path,"nodeids":nodes,"errors":errors}),
        )?)?);
        Ok(())
    }
    pub fn witnesses(&self) -> Value {
        json!(self.attempts.iter().map(|a|json!({"campaign_id":a.campaign_id,"manifest":a.manifest,
            "receipt":a.receipt,"sha256":a.sha256,"native_validated":a.valid,"bindings_current":a.bindings_current,"errors":a.errors})).collect::<Vec<_>>())
    }
}
fn external_failure(node: &str, reason: &str) -> Result<Finding> {
    let path = node.split("::").next().unwrap_or(node);
    let mut row = finding(
        "external-invariant-not-verified",
        format!(
            "{path}: {node} declares an external-invariant waiver that did not verify: {reason}"
        ),
        Some(path),
    )?;
    row.evidence.insert("nodeid".into(), json!(node));
    Ok(row)
}
