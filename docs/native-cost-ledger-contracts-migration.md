# Cost and ledger Python contract migration

The original `src/conductor/test_cost_budget_audit.py` (12 named cases),
`test_cost_ledger.py` (12), and `test_ledger_calibrate.py` (11) were collected
and passed together as 35/35 pytest cases before migration. They had no
parameterized expansions. The matching Rust-owned PyO3 targets are
`python_contracts_cost_budget_audit`, `python_contracts_cost_ledger`, and
`python_contracts_ledger_calibrate`, with shared Rust fixture code in
`tests/python_contracts/cost_contract_support.rs`.

The budget target calls the production Python gate adapter with all five
metric fields, checks its exact status, evidence, and refusal behavior, and
intercepts the subprocess boundary. The ledger target runs the production
Python shim against an executable temporary shell stand-in and checks argv,
exit codes, default path resolution, report output, and missing-input
refusals. The calibration target uses temporary JSONL transcripts and recorded
token-count responses. It checks the ten block-shape rows, compaction-window
inclusion, offline statistics and CLI fixture, the ordered three-call online
path, cache hits, and exact Python payload containers. The recorded provider
is installed only in a test-local `sys.modules` slot and restored afterward;
no network request, real billing, or production ledger data is involved.

Independent review approved all 35 cases after repairing callback argument
binding, a subprocess fixture's recorded arguments, and exact floating-point
assertions. Approximate comparisons remain only where the originals used
`pytest.approx`. The repaired targets passed 12 + 12 + 11 cases before
retirement; scoped Clippy passed after simplifying a fixture type alias.

Exactly these three Python test modules were retired. A scoped source archive
excludes unrelated attribution drafts and contains the same cost-cohort source
and metadata. Its freshly built native extension passed all 24 discovery cases
in `/tmp/forge-native60-cost-discovery-tests.log`; the post-retirement targets
passed all 35 cases in
`/tmp/forge-native60-cost-three-post-retirement-tests.log`. Post-retirement scoped
Clippy passed in `/tmp/forge-native60-cost-three-post-retirement-clippy.log`.
The registry includes
all direct helpers and providers, including native root resolution exercised by
the ledger CLI. This migration changes test ownership and does not by itself
establish runtime speed or behavioral coverage gains. Historical inventory and
metric evidence remain pinned to their own commits.
