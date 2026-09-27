# Native baseline, protected deletion, and snapshot contracts

This cohort ports the executable assertions in three Python test modules to Rust-owned PyO3 targets:

| Original module | Rust contract | Original cases |
| --- | --- | ---: |
| `src/conductor/test_baseline_merge.py` | `python_contracts_baseline_merge.rs` | 9 named, 13 expanded |
| `src/conductor/test_check_protected_deletes.py` | `python_contracts_check_protected_deletes.rs` | 5 |
| `src/conductor/test_commit_snapshot.py` | `python_contracts_commit_snapshot.rs` | 10 |

The Rust targets call production Python APIs directly. Their source assertions, temporary file setup, Git subprocess fixture, and output capture reside in Rust. The protected deletion and snapshot contracts create independent synthetic repositories under temporary directories. They do not operate on this checkout, use Git worktrees, or invoke the repository's commit hooks. The snapshot contract verifies that a private index captures unstaged and untracked content without changing the synthetic repo's shared index.

The baseline contract reads the four present source baselines under `src/conductor/` to preserve the original self-merge shape checks. `src/.secrets.baseline` is absent in this tree: the original parameter row skips, and the corresponding Rust row returns without asserting. The baseline JSON files are inputs, not generated replacements. Historical references to the retired Python test paths in duplication baseline JSON remain provenance data and are not executable imports.

The original three-module baseline was 27 passed and one skipped in `/tmp/forge-native59-baseline-protected-snapshot-original.log`. Independent review found one ordering mismatch in the Rust CLI assertion; it now compares entry keys as a set, as the Python test did. The reviewer gave semantic parity PASS for all 24 named cases and 28 expanded executions. Repaired pre-retirement 13 + 5 + 10 tests and scoped Clippy with `-D warnings` passed in `/tmp/forge-native59-three-pre-retirement-{tests,clippy}.log`.

Exactly the three Python test modules listed above were retired. Post-retirement 13 + 5 + 10 tests and scoped Clippy passed in `/tmp/forge-native59-three-post-retirement-{tests,clippy}.log`. The absent `.secrets.baseline` row remains documented as an optional fixture, not claimed as an exercised baseline shape. A fresh candidate-local native extension passed all 24 discovery checks in `/tmp/forge-native59-discovery-tests.log`; discovery and the three targets passed scoped Clippy in `/tmp/forge-native59-shared-clippy.log`. The registry includes the four present baseline JSON inputs and each directly used helper and provider. Importing the snapshot provider optionally probes `slop_core`, but neither importing it nor executing these ten cases requires that extension.
