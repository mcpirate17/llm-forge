# CRG server and response contracts in Rust

The 17 named response-shim, 8 server, and 4 server-wiring Python cases move to
`native/conductor-native/tests/python_contracts_crg_response_shim.rs`,
`python_contracts_crg_server.rs`, and `python_contracts_crg_server_wiring.rs`.
The original pytest baseline passed 38/38 expanded cases in
`/tmp/forge-crg-server-shim-baseline.log`. Rust owns fixture data, mock callback
binding, calls, expected values, and assertions; PyO3 invokes the production
Python APIs. No Python test module is executed by the Rust targets.

The response target checks payload compaction, path relativization, list caps,
tool wrapping and enrichment, registry pruning, roles, environment overrides,
and the FastMCP version pin. Exact Python container shape is checked before
JSON fixture comparisons, so a tuple cannot silently replace a list. The
server target checks source hashes, pinned CRG compatibility, embedding
provider batches, error propagation, and trace redaction. The wiring target
checks startup order, the disable switch, and fail-loud bridge and shim errors
with exact `pathlib.Path` and call-record comparisons.

The supporting Rust fixtures are `python_contracts/crg_response_support.rs`
and `python_contracts/crg_server_support.rs`, alongside existing shared Rust
`agent_comm_support.rs` and `support.rs`. The discovery registry selects each
target for changes to its direct production providers or included helpers.
The sole test consumer of the former Python server module was a native secret
scanner sentinel; `tests/candidate_checks.rs` now scans the Rust server
contract's equivalent fragmented sentinel fixture and preserves its empty
findings assertion.

Two retained historical references to `src/conductor/test_crg_server.py` are
`campaigns/forge-host-root_crg_server_fest_20260913.json` and
`src/conductor/candidate_review/grandfathered_test_nodeids_61343f57.json`.
They preserve old path and hash provenance, and neither imports or executes the
retired module. A future campaign for current sources and tests must be
regenerated; these records are not current replacement coverage.

Independent source-to-contract parity review passed for all 29 named cases,
including the eleven expanded parameter rows. The three originals were then
retired. Post-retirement exact Rust checks passed 17/17, 8/8, and 4/4;
`candidate_checks` passed 10/10. Scoped Clippy passed with warnings denied.
The exact test and Clippy logs are
`/tmp/forge-crg-server-shim-post-retire-tests.log` and
`/tmp/forge-crg-server-shim-post-retire-clippy.log`.
