# CRG interpreter sync contracts in Rust

The 29 named cases in `src/conductor/test_crg_venv_sync.py` move to
`native/conductor-native/tests/python_contracts_crg_venv_sync_core.rs` (22)
and `python_contracts_crg_venv_sync_session.rs` (7). The original pytest
baseline passed 29/29 in `/tmp/forge-crg-venv-sync-baseline.log`.

Rust builds the temporary source crates, `.mcp.json`, installed-distribution
records, and expected values. The test-only Rust interpreter
`tests/fixtures/crg_venv_sync_child.rs` is symlinked as `fake-python` inside
each checkout. Production `purelib` and `imports_server` still launch it as a
real subprocess, with the original environment-controlled stdout, stderr,
and exit status. A `broken-python` symlink exits 3 for the error case. No
test invokes a real `uv` install. Every case that could reach installation
replaces production `install` with a Rust callback that records calls, updates
the temporary fixture, or fails if the check-only path invokes it.

The targets include `tests/python_contracts/crg_venv_sync_support.rs` and
the existing shared Rust `agent_comm_support.rs` and `support.rs`. Discovery
maps both targets to the production sync, MCP command, project-path, and
native-freshness providers, all direct helpers, and the Rust child binary.
Consumer search found no active import of the retired Python test module.

Independent case-by-case parity review passed for all 29 named cases, including
real subprocess probes and the skipped-install safeguards. The original was
then retired. Post-retirement Rust checks passed 22/22 core and 7/7 session
cases; scoped Clippy for both targets and the Rust child passed with warnings
denied. Logs: `/tmp/forge-crg-venv-sync-post-retire-tests.log` and
`/tmp/forge-crg-venv-sync-post-retire-clippy.log`.
