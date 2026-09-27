# Mull adapter contract migration

`src/conductor/test_mutation_engine_mull.py` has 30 named cases. Its bounded Python baseline was **29 passed, 1 skipped** (`/tmp/forge-mull-baseline.log`). The skip is the live-registry wiring case: this Forge checkout has no registered Mull campaign. That case still checks the registry via `conductor.project_paths` in Rust, then returns early when no Mull manifest exists; a Rust PASS for it does not claim a live Mull campaign was tested.

Rust owns the inputs and assertions in three PyO3 targets:

| Target | Named cases | Contract area |
| --- | ---: | --- |
| `python_contracts_mutation_engine_mull_args.rs` | 6 | Timeout, coverage, toolchain and configure arguments |
| `python_contracts_mutation_engine_mull_rows.rs` | 12 | Elements rows, identity, merge, source scope, checked-in manifest and live-registry precondition |
| `python_contracts_mutation_engine_mull_run.rs` | 12 | Configure/build, per-executable reports, and `execute` refusals and receipt |

`python_contracts/mutation_mull_fixture.rs` builds synthetic Elements reports, loads the checked-in manifest fixture, records subprocess calls, and replaces the build, tool, profile and report entry points. Callbacks preserve the original Python argument binding. All executable work remains fixture-owned: no Mull, CMake build, source mutation, campaign, or worktree is launched. The baseline source and historical test prose describe old monorepo measurements; this migration produces test evidence, not mutation-campaign evidence.

The campaign fixture is `src/conductor/testdata/mull/claude_aria_kernels_fixture.json`. The live-registry case reads repository `campaigns/registry.json` through `conductor.project_paths`. Relevant providers are `mutation_engine_mull.py`, `mutation_engine_generated.py`, `mutation_campaign_model.py`, `mutation_scope.py`, plus `project_paths.py` for the live-registry case and `mutation_run_scope.py` for `execute`. The three targets include the dedicated helper, `agent_comm_support.rs`, and `support.rs`.

Independent parity and consumer review passed after restoring the original
explicit UTF-8 registry read. The original module is retired. Post-retirement
Rust targets passed 6 + 12 + 12 cases in `/tmp/forge-mull-post-retirement-tests.log`,
and scoped Clippy passed with warnings denied in
`/tmp/forge-mull-post-retirement-clippy.log`, using isolated native57 libraries.
The live-registry limitation described above still applies.
