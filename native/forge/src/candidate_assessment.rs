//! Pure evaluation of supplied candidate-review results; not execution proof or approval.
#[path = "candidate_assessment/admission.rs"]
mod admission;
#[path = "candidate_assessment/evaluate.rs"]
mod evaluate;
#[path = "candidate_assessment/model.rs"]
mod model;

use anyhow::Result;
pub use model::{Assessment, CheckResult, CheckStatus, Finding, Severity, StaleException};

/// Assess only supplied results. Completeness and authenticity require a separate orchestrator.
pub fn assess(
    policy: &crate::candidate_policy::Policy,
    candidate: &crate::candidate_source::Candidate,
    results: &[CheckResult],
) -> Result<Assessment> {
    let context = admission::prepare(policy, candidate, results)?;
    evaluate::assess(context, results)
}
