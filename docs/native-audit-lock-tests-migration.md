# Audit-root and governance-lock test migration

The ten assertions formerly in `src/conductor/test_audit_root.py` and
`src/conductor/test_governance_lock.py` now live in the Rust integration targets
`python_contracts_audit_root` and `python_contracts_governance_lock`. Rust owns
the Git fixtures, output and file inspection, exception checks, and assertions.
PyO3 calls the existing Python compatibility API. Each Git repository, commit,
and governance lock is created only under a temporary `Case` directory.

There were no parameterized cases in either Python module. Every named case
maps to one Rust test:

| Original Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_explicit_root_honoured_over_cwd` | `explicit_root_honoured_over_cwd` | Explicit Git root wins over a different invocation repository. |
| `test_default_resolution_uses_cwd_toplevel_not_a_nested_dir` | `default_resolution_uses_cwd_toplevel_not_a_nested_dir` | Nested cwd resolves to its Git toplevel. |
| `test_cwd_outside_worktree_refuses_without_explicit_root` | `cwd_outside_worktree_refuses_without_explicit_root` | `AuditRootError` names the missing Git worktree. |
| `test_explicit_root_must_exist` | `explicit_root_must_exist` | `AuditRootError` reports a missing explicit path. |
| `test_resolved_root_is_printed` | `resolved_root_is_printed` | Stdout contains the resolved root and `git-head=` provenance. |
| `test_mismatch_between_root_and_cwd_toplevel_warns` | `mismatch_between_root_and_cwd_toplevel_warns` | Stderr warns and names both distinct Git roots. |
| `test_no_warning_when_root_matches_cwd_toplevel` | `no_warning_when_root_matches_cwd_toplevel` | Matching Git roots leave stderr empty. |
| `test_released_governance_lock_leaves_no_lease_claim` | `released_governance_lock_leaves_no_lease_claim` | Held JSON has the process PID and lease token; release empties the record. |
| `test_release_does_not_erase_another_holders_record` | `release_does_not_erase_another_holders_record` | A changed peer record survives release byte for byte. |
| `test_an_unreadable_record_does_not_break_release` | `an_unreadable_record_does_not_break_release` | Malformed JSON survives release, and the same fixture lock can be acquired again. |

The two retired test modules have no Python import consumers. Shipped callers of
`conductor.audit_root` and `conductor.candidate_review.engine` stay in place.
The duplication baseline still names `test_audit_root.py` as historical data.
CI's existing `python_contracts_*` selector picks up both targets.

Validation: each target passed `cargo +1.98.0 test --offline --locked` with
`python-compat-tests` and one test thread; scoped Clippy ran with warnings denied.
