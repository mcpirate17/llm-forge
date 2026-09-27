# CRG MCP probe and workspace tool contracts in Rust

The eight cases in `src/conductor/test_crg_mcp_probe.py` and sixteen in
`src/conductor/test_crg_workspace_tools.py` move to
`native/conductor-native/tests/python_contracts_crg_mcp_probe.rs` and
`python_contracts_crg_workspace_tools.rs`. The original pytest baseline passed
24/24 in `/tmp/forge-crg-mcp-workspace-baseline.log`.

Rust owns fixture data, callback behavior, expected values, and assertions.
PyO3 invokes the production Python probe and workspace APIs. The probe's fake
Python worker is replaced by the test-only Rust binary
`tests/fixtures/crg_mcp_probe_child.rs`, which speaks the same JSON-RPC stdio
handshake and supports the original tool-count, path-leak, call-error, and
notification controls. The production probe still creates and closes the real
subprocess; Rust does not mock its transport. Workspace graph fixtures are
synthetic source and SQLite rows built from Rust, with no model or network
calls.

The test targets include `tests/python_contracts/crg_mcp_probe_support.rs`
and `crg_workspace_support.rs`, respectively, plus the existing shared Rust
`agent_comm_support.rs` and `support.rs`. The discovery registry maps each
target's direct production providers and helper files. The MCP child binary is
also a dependency of the probe target.

The historical `campaigns/forge-host-root_crg_workspace_tools_fest_20260913.json`
entry retains the former Python path and hash as provenance. It is neither a
live test-module import nor current replacement coverage; a future campaign
for changed sources and tests must be regenerated.

Independent source-to-contract review passed for all 24 cases after preserving
the original two-element server command, exact error text requirements,
callback binding, and four-result unpacking. The two originals were then
retired. Post-retirement Rust targets passed 8/8 and 16/16, and scoped Clippy
for both targets and the child binary passed with warnings denied. Logs:
`/tmp/forge-crg-mcp-workspace-post-retire-tests.log` and
`/tmp/forge-crg-mcp-workspace-post-retire-clippy.log`.
