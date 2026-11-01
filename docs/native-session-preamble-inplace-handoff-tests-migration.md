# Session preamble and in-place handoff contract migration

Handoff now runs directly through `forge handoff`. Six `native_handoff` integration
tests cover activation/idempotency/lineage, stale and corrupt data, locking and
hostile file types, malformed metadata and input bounds, preparation/identity/
Unicode clipping, and resource IDs. `native_session_brief` additionally verifies
default brief composition and explicit context preservation. The Python handoff
module, its PyO3 target and dedicated helper have been deleted, not archived.

The 18 original session-preamble cases retain transitional PyO3 coverage in:

- `native/conductor-native/tests/python_contracts_session_preamble.rs`
- `native/conductor-native/tests/python_contracts/session_preamble_support.rs`

This target calls the shipped Python API through PyO3. It does not invoke the
retired pytest files. The shared `python_contracts/support.rs` fixture restores
process state between cases, and the preamble helper restores its temporary
`sys.modules` replacement after refresh tests.

The earlier 26-case Python/PyO3 baseline is historical evidence, not current
native acceptance. Native startup and brief tests run with an empty PATH and
invalid interpreter selection. See [native task briefs](native-session-brief.md)
and [native lifecycle](native-lifecycle.md) for current interfaces and bounds.
