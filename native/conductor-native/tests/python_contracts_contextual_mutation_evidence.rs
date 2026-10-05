#![cfg(feature = "python-compat-tests")]
//! Final-hash contextual admission keeps original independent witnesses.
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::ffi::CString;
use support::{module, Case};

const FIXTURE: &str = r#"
from copy import deepcopy
from types import SimpleNamespace
from conductor.candidate_review.contextual_mutation_evidence import (
    ContextWitness, complete_pass_errors, contextual_admission_errors, _summary_current,
)
NODE = 'tests/test_flow.py::test_flow'
H = 'a' * 64
RUNNER = {'mutation_testing_sha256': H, 'components': {'runner.py': H}}
def witness(source='train.py', classification='CORE', stamp='2026-10-05T00:00:00+00:00', campaign=None):
    receipt = {
        'campaign_id': campaign or source, 'manifest_sha256': H,
        'source_sha256': {source: H}, 'test_sha256': {'tests/test_flow.py': H},
        'runner_sha256': H, 'runner_components_sha256': RUNNER['components'],
        'generated_at': stamp, 'status': 'PASS',
        'baseline': {'returncode': 0, 'timed_out': False},
        'outcome_counts': {'KILLED': 1, 'NO_COVERAGE': 0, 'SURVIVED': 0, 'TIMED_OUT': 0, 'ERROR': 0, 'UNVIABLE': 0},
        'attribution': {'status': 'ATTRIBUTED', 'attributed_mutants': 1, 'killed_mutants': 1},
        'mutants': [{'id': 'm', 'path': source, 'outcome': 'KILLED'}],
        'test_value': {'schema_version': 'llm.mutation-testing.test-value.v1', 'status': 'PASS',
            'tests': [{'nodeid': NODE, 'classification': classification, 'killed_mutants': ['m']}]},
    }
    return ContextWitness(source + '.json', H, receipt, True, True)
def changed(original, *, native=True, current=True, **updates):
    receipt = deepcopy(original.receipt)
    receipt.update(updates)
    return ContextWitness(original.receipt_path, H, receipt, native, current)
"#;

fn check(case_code: &str) {
    let _case = Case::new();
    Python::attach(|py| {
        module(
            py,
            "conductor.candidate_review.contextual_mutation_evidence",
        );
        let code = CString::new(format!("{FIXTURE}\n{case_code}\n")).unwrap();
        PyModule::from_code(py, &code, c"contextual_case.py", c"contextual_case")
            .unwrap_or_else(|error| panic!("context admission regression: {error}"));
    });
}

#[test]
fn newer_other_context_does_not_erase_complete_positive_original() {
    check("a=witness(); b=witness('selection.py', 'MERGE', '2026-10-05T01:00:00+00:00'); assert contextual_admission_errors(NODE, ['train.py'], [a,b]) == []; assert b.receipt['test_value']['tests'][0]['classification']=='MERGE'");
}
#[test]
fn every_explicit_required_source_needs_positive_evidence() {
    check("a=witness(); b=witness('selection.py','MERGE'); errors=contextual_admission_errors(NODE,['train.py','selection.py'],[a,b]); assert len(errors)==1 and 'selection.py' in errors[0]");
}
#[test]
fn newer_failed_or_partial_same_context_blocks_old_pass() {
    check("a=witness(); b=changed(a, native=False, generated_at='2026-10-05T01:00:00+00:00', status='BASELINE_FAILED'); assert contextual_admission_errors(NODE,['train.py'],[a,b])");
}
#[test]
fn contradictory_same_final_context_is_blocking() {
    check("a=witness(); b=witness(classification='MERGE'); errors=contextual_admission_errors(NODE,['train.py'],[a,b]); assert errors and 'conflicting' in errors[0]");
}
#[test]
fn source_test_manifest_runner_drift_and_unregistered_witness_are_rejected() {
    check("a=witness(); c=SimpleNamespace(campaign_id='train.py', manifest_sha256=H, source_sha256={'train.py':H},test_sha256={'tests/test_flow.py':H}); assert _summary_current(a.receipt,c,RUNNER); fields=['source_sha256','test_sha256','manifest_sha256','runner_sha256','runner_components_sha256','campaign_id'];\nfor field in fields:\n b=changed(a, **{field: {}}); assert not _summary_current(b.receipt,c,RUNNER)\nassert contextual_admission_errors(NODE,['train.py'],[changed(a,native=False)])");
}
#[test]
fn partial_node_inventory_and_stale_test_binding_cannot_admit() {
    check("a=witness(); b=changed(a,test_value={'schema_version':'llm.mutation-testing.test-value.v1','status':'PASS','tests':[]}); assert contextual_admission_errors(NODE,['train.py'],[b]); assert contextual_admission_errors(NODE,['train.py'],[changed(a,current=False)])");
}
#[test]
fn nominal_pass_with_timeouts_and_no_attribution_is_blocked() {
    check("a=witness(); counts=deepcopy(a.receipt['outcome_counts']); counts.update(KILLED=316,TIMED_OUT=6); b=changed(a,outcome_counts=counts,attribution={'status':'NO_ATTRIBUTION','killed_mutants':316,'attributed_mutants':0}); assert complete_pass_errors(b.receipt); assert contextual_admission_errors(NODE,['train.py'],[b])");
}
#[test]
fn unrelated_source_positive_cannot_clear_required_source_gap() {
    check("assert contextual_admission_errors(NODE,['required.py'],[witness('other.py')]); assert contextual_admission_errors(NODE,[],[witness()])");
}
#[test]
fn unexpandable_or_failed_native_original_cannot_be_used_as_a_witness() {
    check("from conductor.mutation_receipt_slim import expand_receipt, ReceiptDetailError\nfor detail in [{'encoding':'superseded','superseded_by':'other.json'}, {'encoding':'invalid'}]:\n try: expand_receipt({'detail':detail})\n except ReceiptDetailError: pass\n else: raise AssertionError('unverified receipt detail accepted')\nassert contextual_admission_errors(NODE,['train.py'],[changed(witness(),native=False)])");
}
