# Graph-refresh state tests in Rust

The thirteen remaining named and expanded cases in
`src/tooling/hooks/agent/test_crg_refresh_state.py` moved to
`native/conductor-native/tests/python_contracts_crg_refresh_state.rs`.
The original pytest baseline passed 13/13 in
`/tmp/forge-crg-refresh-baseline.log`. Rust owns the temporary store and repo,
lock handles, process arguments, callback state, expected values, and
assertions. PyO3 calls the production `crg_refresh_state`,
`crg_graph_refresh`, and dispatch doctor APIs. No Python test module or
Python test-worker script is executed.

| Original case (without `test_`) | Preserved behavior |
| --- | --- |
| `request_spawns_one_worker_then_queues` | A real child takes the worker lock; the second request queues and pending paths deduplicate in order. |
| `drain_refuses_while_another_worker_holds_the_lock` | A separate open file description holds the lock; drain returns zero without calling refresh or deleting pending. |
| `drain_records_a_raising_refresh_and_keeps_going` | A Rust callback raises `RuntimeError`, then a second batch runs; the failure notice has exact text and is consumed once. |
| `failure_output_surfaces_once_as_a_system_message` | A stored `ValueError` reaches the next hook event with its path, then disappears. |
| `drain_debounces_and_coalesces_into_one_refresh` | A second write during the first sleep coalesces and deduplicates; both 0.25-second sleep calls are observed. |
| `hook_output_queues_only_graph_files` | The Python edit queues once, the Markdown edit does not, and a missing graph tool reports its warning. |
| `status_and_doctor_report_a_stuck_lock` | A held lock is initially healthy; after 301 seconds of simulated age, status and doctor report a DEAD stuck worker. |
| `doctor_warns_on_a_waiting_failure_and_orphaned_pending` | A failure notice is WARN; a 60-second pending marker without a worker is DEAD. |
| `detached_worker_drains_and_wait_sees_fresh` | A real detached child calls production `drain` through embedded PyO3, writes both queued paths, and bounded wait observes fresh state. |
| `hung_batch_child_is_killed_and_recorded` | A five-second Rust child exceeds the 0.3-second batch limit; drain records a failure and returns within three seconds. |
| `batch_child_warning_reaches_the_next_hook_event` | The child prints the original warning; the next event surfaces it without FAILED and consumes it once. |
| `batch_child_failure_is_recorded_not_lost` | The child exits one with the original not-installed message; the next event reports STALE. |
| `legacy_wiring_refreshes_synchronously` | The no-argument body writes a marker, leaves no pending queue, prints exact JSON, and reports a subsequent child failure inline. |

`tests/fixtures/crg_refresh_child.rs` is a feature-gated, test-only Cargo
binary (`test = false`, `bench = false`). Its hold-lock, embedded production
drain, sleep, warning, failure, and marker modes provide real process behavior
without contaminating libtest stdout. The integration target obtains its path
from `CARGO_BIN_EXE_crg_refresh_child`. The shared Rust helper builds store
fixtures and restores patched Python attributes and environment state.

The discovery registry maps the production refresh-state and hook modules,
dispatch doctor, both `#[path]` Rust helpers, and the child fixture to this
target. Changing the child fixture selects its Cargo contract. A consumer
search found no import of the retired Python test module or its worker-string
fixtures. Three older graph-refresh wait contracts already live in
`native/forge/src/crg_refresh.rs`; they are separate from these thirteen cases.

The exact Rust target passed 13/13, and scoped Clippy passed with warnings
denied. The Python original was retired only after independent parity review
and post-retirement exact checks.
