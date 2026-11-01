# CRG server and response migration record

This records the historical migration of 17 response-shim, 8 server, and 4
server-wiring Python cases to Rust-owned compatibility targets. At migration
time, the original pytest baseline passed 38/38 expanded cases in
`/tmp/forge-crg-server-shim-baseline.log`. The targets then invoked the
production Python APIs through PyO3. Those Python adapters and their dedicated
compatibility targets have since been retired; the historical counts below do
not describe current coverage.

The migration moved these cases to
`native/conductor-native/tests/python_contracts_crg_response_shim.rs`,
`python_contracts_crg_server.rs`, and `python_contracts_crg_server_wiring.rs`.
Rust owned fixture data, mock callback binding, calls, expected values, and
assertions. No Python test module was executed by those Rust targets.

The retired response target checked payload compaction, path relativization,
list caps, tool wrapping and enrichment, registry pruning, roles, environment
overrides, and the FastMCP version pin. It checked exact Python container shape
before JSON fixture comparisons. The retired server target checked source
hashes, pinned CRG compatibility, embedding provider batches, error
propagation, and trace redaction. The retired wiring target checked startup
order, the disable switch, and fail-loud bridge and shim errors with exact
`pathlib.Path` and call-record comparisons.

The historical supporting Rust fixtures were
`python_contracts/crg_response_support.rs` and
`python_contracts/crg_server_support.rs`, alongside shared Rust
`agent_comm_support.rs` and `support.rs`. At that time, the discovery registry
selected each target for changes to its direct production providers or included
helpers.
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

At migration time, independent source-to-contract parity review passed for all
29 named cases, including eleven expanded parameter rows. The three original
Python suites were then retired. The post-retirement checks recorded at that
time passed 17/17, 8/8, and 4/4; `candidate_checks` passed 10/10, and scoped
Clippy passed with warnings denied. These results are historical; the removed
compatibility targets are not active coverage. The old logs are
`/tmp/forge-crg-server-shim-post-retire-tests.log` and
`/tmp/forge-crg-server-shim-post-retire-clippy.log`.

## Current native surfaces

The persistent MCP service is provided by Forge's native `harness stdio`
adapter. Its inspection tools are `code_inspect`, `code_fold`, and
`code_search`; workspace context is available through `session_brief` and
`workspace_recall`. Native session-brief and mailbox commands provide the
corresponding CLI surfaces, including `forge session brief --cards-index` for
an operator-selected cards index. These native interfaces have their own
coverage; they do not exercise the retired Python adapters. The exact legacy
`conductor.crg_server` configuration matcher remains in `forge mcp` so
`--replace-legacy` can remove only the recognized historical entry.
