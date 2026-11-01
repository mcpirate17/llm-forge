# Cost and ledger contract migration

The retired Python test cohort had 35 named cases: `test_cost_budget_audit.py`
(12), `test_cost_ledger.py` (12), and `test_ledger_calibrate.py` (11). Those
cases passed together before migration; these counts are historical. The
Python budget and forwarding adapters have since been retired. Host operations
now use `forge ledger budget rollup|audit|report|gate --host HOST`; the separate
per-tier routing report remains `forge ledger report`.

Native CLI coverage lives with Forge, including
`native/forge/tests/native_cost_budget.rs`. The previous Python compatibility
targets for the budget adapter and cost-ledger forwarding shim are retired.
Calibration still has a separate native/PyO contract target. Do not treat the
retired Python target names as current validation targets.

The five native budget CLI tests pass with an empty `PATH`, covering measured
statuses, gate evidence, explicit baseline recording, malformed baseline
refusal, reporting, transcript defaults and content privacy. Six native claim
tests cover lifecycle and concurrency behavior. These checks do not establish
runtime speed gains or completion of the broader native migration.

The ledger state default is `XDG_STATE_HOME/llm-forge/ledger`, falling back to
`HOME/.local/state/llm-forge/ledger`; `LEDGER_ROOT` or an explicit option can
override it. Rollup does not implicitly fetch CI history. The legacy
`make ci-history` target remains an explicit opt-in operation.
