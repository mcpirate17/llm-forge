# Candidate review built-in checks: native boundary

`src/conductor/candidate_review/checks.py` remains the public review interface. It owns
`ReviewContext`, `TestSelection`, `CheckResult` timing, `Finding` construction and
fingerprints, snapshot reads, claim-store I/O, and parser errors that must retain
CPython or third-party wording. The production decisions for candidate integrity,
dependency pairing, secret detection, native-source unsafe calls, performance and
research evidence, policy file selection, and Python AST rules live in
`native/conductor-native/src/candidate_checks*.rs`.
Dependency findings retain the existing manifest-kind priority and sort paths
within each kind so the same candidate produces the same finding order.

The private `_native.candidate_checks_native(operation, payload_json)` seam passes
only the data each rule needs. Its result is ordered finding data; Python converts
severity strings to the public `Severity` enum before `_result` computes the
fingerprint. The Rust AST check uses the shipped Ruff parser and its token stream,
including comment tokens. Python still calls `ast.parse` before it to preserve the
existing `python-parse` failure wording and line attribution. The direct
`_PythonVisitor` and `_call_name` test seams remain available without a second
Python rule engine.

Rust tests in `native/conductor-native/tests/candidate_checks.rs` exercise actual
native decisions with ordered rule, message, severity, path, line, help, and
evidence assertions. Decorated function and class traversal keeps CPython's
finding order, and function findings locate the `def` token even when Ruff's
statement range begins at a decorator.

| Native behavior | Rust coverage | Python interface coverage |
| --- | --- | --- |
| Tree/change integrity, protected artifacts, symlinks, size limits | `integrity_preserves_tree_first_order_and_change_evidence`, `deletion_and_symlink_admission_keep_distinct_attribution`, `fingerprint_inputs_match_the_previous_python_scan_contract` | `test_adversarial_builtin_matrix_exercises_real_candidate_flows` and tree integrity cases in `test_candidate_review.py` |
| Dependency pairing and policy file selection | `dependency_pairing_and_file_selection_preserve_path_scope` | Candidate review policy and dependency cases in `test_candidate_review.py` |
| Secret and unsafe native API scans | `scans_preserve_rule_order_line_numbers_and_messages`, `crg_server_test_sentinel_does_not_match_native_secret_patterns` | Adversarial matrix and `test_candidate_review_scan_coverage.py` |
| Performance and research evidence | `performance_and_research_decisions_keep_evidence_contract` | Research changed-line cases in `test_candidate_review.py` |
| Python AST rules and comment tokens | `python_ast_uses_syntax_and_comment_tokens_and_preserves_order`, `decorated_function_keeps_cpython_finding_order_and_def_location` | AST cases in `test_candidate_review.py` and `test_candidate_review_call_and_evidence.py` |

The old `test_crg_test_sentinel_literal_avoids_secret_scan_trip` Python test
read `SECRET_PATTERNS` directly. Its exact fixture check now runs through the
native secret scanner in the Rust test named above. Other Python tests remain
because they exercise public result and receipt flows beyond the native rule
decisions.
