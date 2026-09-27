# Native freshness and Bash hook parity contracts

This bounded cohort moves 28 named Python tests to Rust-owned PyO3 contracts:
26 cases from `src/tooling/hooks/dispatch/test_native_freshness.py` and two
from `src/tooling/hooks/claude/test_bash_pretooluse_hooks_parity_corpus.py`.
The latter executes all 64 descriptors in the shared frozen corpus. Both
Python baselines passed before retirement (26/26 and 2/2).

`python_contracts_native_freshness.rs` constructs the same synthetic crate,
venv, metadata, source, and stamp fixtures in Rust. It calls the existing
`tooling.hooks.dispatch.native_freshness`, adapter, and registry APIs through
PyO3. Each original assertion remains Rust-owned, including exact crate and
finding collections, report strings, digest boundaries, adapter failure
behavior, and the SessionStart registration.

`python_contracts_bash_pretooluse_parity.rs` reads the same
`native/forge/tests/fixtures/bash_pretooluse_{corpus,expected}.json` files as
the original Python suite and native Forge parity test. Rust builds each
scratch repository and claims/marker state, dispatches the existing Python
hook production functions through PyO3, and compares every live verdict to
the frozen expected value. The fixture creates Git repositories with
`git init`; it does not create Git worktrees. The 0.8.1 Forge baseline binary
at `/tmp/forge-native-baselines/ca95e41/forge` is required for legacy hook
operations; the older a755941 baseline lacks that command.

The source-to-target registry includes these dependencies:

| Source | Rust target |
| --- | --- |
| `src/tooling/hooks/dispatch/native_freshness.py` | `python_contracts_native_freshness` |
| `src/tooling/hooks/dispatch/adapters.py` | `python_contracts_native_freshness` |
| `src/tooling/hooks/dispatch/registry.py` | `python_contracts_native_freshness` |
| `src/conductor/project_paths.py` | `python_contracts_native_freshness` |
| `src/conductor/crg_venv_sync.py` | `python_contracts_native_freshness` |
| `src/tooling/hooks/agent/crg_gate.py` | `python_contracts_bash_pretooluse_parity` |
| `src/tooling/hooks/agent/crg_graph_refresh.py` | `python_contracts_bash_pretooluse_parity` |
| `src/conductor/current_work_guard.py` | `python_contracts_bash_pretooluse_parity` |
| `src/conductor/candidate_review/identity.py` | `python_contracts_bash_pretooluse_parity` |
| `native/forge/tests/fixtures/bash_pretooluse_corpus.json` and `bash_pretooluse_expected.json` | `python_contracts_bash_pretooluse_parity` |

Each target's dedicated helper maps to its target. The shared `support.rs`
maps to both, and `agent_comm_support.rs` maps to Bash parity. The registry
is maintained separately by the parent migration lane.

The workspace exposure parity suite is deferred because its fixture creates
Git worktrees, which Forge's working contract prohibits. The larger post-tool
parity suite is also outside this cohort. No production imports of either
Python test module were found; historical campaign data is retained.

Independent source-to-contract review passed all 28 named cases and the 64
corpus descriptors before the two Python originals were retired. After
retirement, the exact Rust targets passed 26/26 and 2/2 with the isolated
`conductor_native` 0.1.50 extension and pinned Forge 0.8.1 binary
(`/tmp/forge-native-freshness-bash-postretire-tests.log`). Scoped Clippy with
`-D warnings` passed
(`/tmp/forge-native-freshness-bash-postretire-clippy.log`). The old corpus
driver fixture is retained as historical test data; it has no production
consumer.
