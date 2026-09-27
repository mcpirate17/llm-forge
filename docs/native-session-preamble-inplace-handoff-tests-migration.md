# Session preamble and in-place handoff contract migration

The 18 tests in `src/conductor/test_session_preamble.py` and the 8 tests in
`src/conductor/test_inplace_handoff.py` are ported to Rust-owned PyO3 integration
targets. Their assertions, fixture state, policy text, file setup, and output
checks live in:

- `native/conductor-native/tests/python_contracts_session_preamble.rs`
- `native/conductor-native/tests/python_contracts_inplace_handoff.rs`
- `native/conductor-native/tests/python_contracts/session_preamble_support.rs`
- `native/conductor-native/tests/python_contracts/inplace_handoff_support.rs`

The targets call the shipped Python APIs through PyO3. They do not invoke the
retired pytest files. The shared `python_contracts/support.rs` fixture restores
process state between cases, and the preamble helper restores its temporary
`sys.modules` replacement after refresh tests.

Before retirement, the exact Python baseline passed 26/26. The Rust targets
passed 18/18 and 8/8 with `--features python-compat-tests --test-threads=1`;
scoped Clippy passed with `-D warnings`. The fixture keeps claims, policy,
identity, CLI output, and file paths local to each test. The registry maps the
relevant Python providers and Rust helpers to both targets so source changes
select these contracts in candidate and graph test runs.
