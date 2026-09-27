# Candidate verification native boundary

`conductor.candidate_review.verification` keeps its public Python entry points,
`Finding` and `TestSelection` results, and the test seams used by host projects.
Python still reads candidate and anchored Git blobs, checks pinned file hashes,
queries the dependency graph, loads mutation receipts and manifests, evaluates
external invariants, applies value waivers, and runs targeted tests. These
operations depend on the candidate snapshot or the host environment.

`conductor_native.candidate_verification_ast_native(source, path)` collects
top-level and `Test*` class test definitions, including decorators in each
definition's text. `candidate_verification_native(operation, request_json)`
handles deterministic decisions over JSON values: anchored inventory schema,
path and tree-inventory agreement; tombstone exclusions; changed-definition
nodeids; mutation-waiver states; test selection; evidence-row findings and
indexing; value-admission planning; required receipt paths; and evidence
metrics. Its Rust functions can run without a Python interpreter. The Python
adapter constructs the same public dataclasses and preserves the existing
finding order, text, severity, path, help, evidence, and fingerprints.

The inventory bytes, anchor commit, and anchor tree still need their original
SHA-256 and Git proofs before native schema validation. A malformed or
unprovable inventory fails closed. The Rust decision excludes tombstoned
paths even if a test file is recreated; the Python boundary checks which
otherwise valid paths are present in the host repository. Candidate test
definitions are compared with their base blob, so an unchanged definition in
a modified file does not acquire a new value-evidence obligation.

| Behavior | Direct Rust contract | Python boundary contract |
| --- | --- | --- |
| Decorator-inclusive test identity and anchored inventory | `candidate_verification::ast_collects_decorated_top_level_and_test_class_definitions_only`, `anchored_inventory_rejects_drift_and_excludes_tombstones` | `python_contracts_candidate_verification::definitions_receipt_findings_and_scope_keep_python_shapes`; existing candidate review hardening tests |
| Changed-definition value gate and receipt scope | `candidate_verification::value_gate_charges_only_new_or_changed_definitions`, `evidence_rows_fail_closed_without_losing_order_or_metrics` | `python_contracts_candidate_verification::definitions_receipt_findings_and_scope_keep_python_shapes`; `test_receipt_scope.py` |
| Graph, convention, changed-test and crate-local selection | `candidate_verification::selection_keeps_native_evidence_out_of_pytest_and_reports_uncovered_python` | `python_contracts_candidate_verification::selection_adapter_preserves_graph_failure_and_structured_finding`; `test_native_test_selection.py` |
| Waiver activation and evidence admission | `candidate_verification::waiver_decision_preserves_base_file_and_source_failure_reasons`, `evidence_rows_fail_closed_without_losing_order_or_metrics` | `python_contracts_candidate_verification::value_admission_reads_slim_receipt_and_blocks_malformed_envelopes`; `test_verification.py`; `test_mutation_value_gate_edges.py` |
| Malformed native request | Native `Result` rejects unknown operations | `python_contracts_candidate_verification::malformed_native_request_fails_closed` |

The existing Python tests remain active because they exercise host Git and
filesystem proofs, graph and receipt integrations, runtime invariants, and
public compatibility paths. No historical inventory, campaign, or receipt is
rewritten by this migration.

Focused validation passed on 2026-09-27: six direct native tests, four PyO3
compatibility tests, and the existing candidate review regression selection
(46 passed, one skipped). Scoped Clippy passed with warnings denied for the
library and both candidate verification test targets.
