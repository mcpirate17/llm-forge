# Native workspace evaluation contracts

`src/conductor/test_workspace_eval.py` has four Rust-owned PyO3 equivalents in
`native/conductor-native/tests/python_contracts_workspace_eval.rs`. The original
Python suite was retired after independent review and successful native
validation. No production evaluation behavior changed.

| Python case | Rust case | Preserved outcome |
| --- | --- | --- |
| `test_offline_eval_is_honestly_not_ready` | `offline_eval_is_honestly_not_ready` | `NOT_READY`, invalid, score below 100, embedding-canary diagnostic |
| `test_complete_runtime_receipt_is_required_for_pass` | `complete_runtime_receipt_is_required_for_pass` | Full required-cell receipt passes with valid, 100-point result |
| `test_missing_or_nonpass_runtime_cell_fails_closed` | `missing_or_nonpass_runtime_cell_fails_closed` | Incomplete receipt containing a non-PASS cell is invalid and `FAIL-CLOSED` |
| `test_write_receipt_preserves_status` | `write_receipt_preserves_status` | Written JSON retains result status and runtime-matrix oracle provenance |

The two offline cases use the existing native `HookMatrix` to construct a
four-agent temporary repository. `workspace_runtime_support.check_hook_sources`
parses three fixture paths ending in `.py` as Python source, and
`check_hook_noops` executes the two Obsidian programs. Those inputs must remain
source files for the original runtime control to pass. The separate
`workspace_source_fixture.rs` overlays the gate launcher and two Obsidian
programs with the original `conftest.py` source bodies, now stored under
`src/conductor/testdata/workspace_hooks/`. The selected interpreter fills the
original Obsidian shebang. This is fixture input only: construction, cleanup,
calls, and assertions stay in Rust. The native child launcher remains in the
shared `HookMatrix` for suites that do not inspect `.py` source.

`python_contracts_workspace_runtime_reconcile.rs` also uses that source overlay
for its hook-program check and receipt reconciliation. It no longer imports
`pytest` or `conductor.conftest`; the obsolete
`python_contracts/workspace_runtime_fixture.rs` wrapper was removed after an
exact reference search found only that target. Other reconciliation assertions
and preserved receipt evidence remain unchanged.

All temporary repositories and receipts live under `Case` and are removed by
its guard. Offline evaluation invokes four local hook programs, reads an active
state, executes launcher `--version` checks, and uses the configured Grok
inspection stub. It does not invoke embedding or retrieval backends, model
calls, network checks, or GPU work. Receipt-backed evaluation only parses the
temporary JSON receipt. The test provider is the Forge Python environment and
the production `conductor` modules; no original test module is imported.

The bounded original Python baseline passed 4/4 cases using the fresh native
extension (`/tmp/forge-workspace-eval-python-baseline.log`). The scoped Cargo
run passed 4/4 new eval cases and 4/4 reconciliation cases with two workers and
CUDA masked (`/tmp/forge-workspace-eval-cargo-test.log`). Scoped Clippy with
`-D warnings` passed for both targets
(`/tmp/forge-workspace-eval-clippy.log`). The integrated cohort also passed
31 affected cases and 27 discovery cases against rebuilt native extensions.
The follow-up fixture-evidence check and both fixture-boundary cases passed
after the selection correction, bringing discovery coverage to 28 distinct cases.
All 11 candidate-runner cases and scoped Clippy also passed.

Discovery maps the production providers, three direct Rust helpers, native
child source, and both Python fixture inputs. Registered Python files under
`src/conductor/testdata/` select their dependent contracts without appearing
in the registered production-source inventory. The selection plan includes
mapped Python fixture paths as evidence inputs, so candidate verification
recognizes their selected Rust contracts. This field is separate from changed-line
coverage; unmapped Python inputs still produce a missing-test finding.
Fixture inputs must exist as regular files inside
the repository; missing files, directories, external symlinks, and registry
path escapes fail closed. The two inputs remain source-language test data,
counted separately from executable test suites.
