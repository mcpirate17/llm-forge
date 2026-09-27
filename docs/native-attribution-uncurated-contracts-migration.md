# Native attribution and uncurated scorer contracts

This cohort moves three executable Python suites to Rust-owned PyO3 assertions:

| Original test module | Rust target | Executable cases |
| --- | --- | ---: |
| `src/conductor/test_finding_attribution.py` | `python_contracts_finding_attribution.rs` | 15 |
| `src/conductor/test_killer_attribution.py` | `python_contracts_killer_attribution.rs` | 11 |
| `src/conductor/test_uncurated_kill_rate.py` | `python_contracts_uncurated_kill_rate.rs` | 21 |

The focused original baseline passed 47/47 cases in `/tmp/forge-native60-attribution-original-baseline.log`. The uncurated module also contains two `def test_*` lines inside its `SUBJECT_TESTS` fixture string; those are fixture input for a subprocess, not collected cases in this cohort.

Rust owns the fixture construction, source/assertion logic, campaign manifest input, and output checks. The legacy uncurated scorer exercises automatic single-site AST generation against a synthetic temporary subject and launches local pytest/coverage subprocesses for that fixture. Its original subject and test source strings are preserved verbatim as Rust constants. Production `measure` temporarily rewrites only that temporary subject, then restores it; no checkout source, worktree, registered mutation campaign, or receipt is modified. The test verifies the scorer's behavior and is not a mutation-evidence receipt.

Production dependencies are `candidate_review/{engine,model,policy}.py` for finding attribution; `mutation_testing.py`, `mutation_campaign_model.py`, `mutation_value.py`, and the native attribution bridge for killer attribution; and `uncurated_kill_rate.py` plus `project_paths.py` for campaign resolution. Rust test-support dependencies are the three dedicated `python_contracts/*_support.rs` files and the existing `python_contracts/support.rs`. No active executable imports of the original three test modules were found.

Independent review approved all 47 cases after restoring the originals' all-keyword constructor calls for `Finding` and `Mutation`. Review also independently confirmed the fixture strings' exact 109/155 bytes and each assertion's Python container and identity semantics.

A fresh candidate-local extension passed the repaired 47 cases and all 24 discovery checks in `/tmp/forge-native61-attribution-pre-retirement-tests.log`; scoped Clippy passed before retirement. Exactly the three original Python test modules listed above were retired. Post-retirement 15 + 11 + 21 cases passed in `/tmp/forge-native61-attribution-post-retirement-tests.log`, and scoped Clippy passed in `/tmp/forge-native61-attribution-post-retirement-clippy.log`. The registry includes directly used helpers, eager import providers, native attribution logic, and native project-path resolution. These checks establish contract parity for this cohort, not an overall runtime speedup or mutation-evidence receipt.
