# Dispatch doctor and merge contract migration

`src/tooling/hooks/dispatch/test_doctor.py` contains 12 named tests and 13
expanded executions. `test_merge.py` contains 21 named tests. Before the port,
the two suites passed together: 34 executions in
`/tmp/forge-dispatch-doctor-merge-baseline.log`.

`native/conductor-native/tests/python_contracts_dispatch_doctor.rs` calls the
existing `tooling.hooks.dispatch.doctor` API through PyO3. Rust creates each
hook script, mode, settings file, and scratch directory, then owns every
assertion. Its 12 cases preserve the original names and test the static and
run diagnoses, exit statuses, malformed output, timeout, script types,
command resolution, CLI output, and both parameterized session events.

`native/conductor-native/tests/python_contracts_dispatch_merge.rs` calls the
existing `tooling.hooks.dispatch.merge` API through PyO3. Its 21 cases preserve
the original names and cover vote precedence, exact quiet/schema output,
ordered reasons and contexts, error policy, legacy decisions, stopping,
updated output conflicts, and unsupported hook outputs. Rust owns fixture
construction and assertions; no Python test algorithms are introduced.

The source dependencies for discovery are:

| Source path | Cargo target |
| --- | --- |
| `src/tooling/hooks/dispatch/doctor.py` | `python_contracts_dispatch_doctor` |
| `src/tooling/hooks/dispatch/registry.py` | `python_contracts_dispatch_doctor` |
| `src/tooling/hooks/dispatch/payloads.py` | `python_contracts_dispatch_doctor` |
| `src/tooling/hooks/agent/crg_refresh_state.py` | `python_contracts_dispatch_doctor` |
| `src/tooling/hooks/dispatch/merge.py` | `python_contracts_dispatch_merge` |
| `native/conductor-native/tests/python_contracts/support.rs` | Both targets |
| `native/conductor-native/tests/python_contracts/agent_comm_support.rs` | Both targets |

There are no production imports of either retired test module. Historical
campaign receipts remain untouched.

Independent parity review passed in
`/tmp/forge-dispatch-doctor-merge-review.md`. Both original test modules were
retired after that review. After retirement, the exact Rust targets passed
12/12 and 21/21 tests in
`/tmp/forge-dispatch-doctor-merge-postretire-tests.log`; scoped Clippy with
`-D warnings` passed in
`/tmp/forge-dispatch-doctor-merge-postretire-clippy.log`.
