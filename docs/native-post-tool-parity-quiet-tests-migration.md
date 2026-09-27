# PostToolUse Python contract migration

The two named tests in `src/tooling/hooks/claude/test_post_tool_parity_corpus.py`
and the three in `src/tooling/hooks/claude/test_post_tool_quiet.py` are ported to
Rust-owned PyO3 targets. The exact Python baseline passed 5/5 before retirement.
The corpus target executes all 44 descriptors and compares each live Python
production API result with the shared frozen expected fixture. Its fixtures,
environment setup, normalization, and assertions live in Rust:

- `native/conductor-native/tests/python_contracts_post_tool_parity.rs`
- `native/conductor-native/tests/python_contracts/post_tool_parity_support.rs`
- `native/conductor-native/tests/python_contracts/post_tool_parity_cases.rs`
- `native/conductor-native/tests/python_contracts/post_tool_parity_obsidian.rs`

The quiet target, `native/conductor-native/tests/python_contracts_post_tool_quiet.rs`,
calls the shipped Python binding through PyO3 and checks the Read continuation,
malformed-response diagnostic, and Bash save-directory behavior. Both targets
also include the existing Rust `support.rs` and `agent_comm_support.rs` fixtures.

All 44 corpus descriptors use scratch directories or a plain `.git` marker.
None creates a Git worktree or clone. The corpus preserves its nine fixture
kinds: graph reports, graph Bash and edit events, read budget, telemetry
records and paths, post-edit audit, and Obsidian session memory. Stub formatters
and the inert `/bin/sleep` worker keep the fixture bounded. The Obsidian case
clears only its own session accumulator file before and after each call.

The corpus and quiet targets each map their production providers, shared
fixtures, and Rust helpers in `python_contract_targets.tsv`. The retired Bash
PreToolUse pytest module was the sole repository consumer of
`native/forge/tests/fixtures/parity_driver.py`; a recursive tracked-source
search found no remaining reference, and no process was running it. That
obsolete Python test harness was removed separately. The native Forge Bash
parity test now names its Rust PyO3 counterpart in its header.

Independent source-to-Rust review found no remaining parity gap after the
inert worker callback was bound to the original `_body, _root` Python
signature. Both original PostToolUse pytest modules were then retired. The
post-retirement exact Rust targets passed 2/2 and 3/3, exercising all 44
corpus descriptors; scoped Clippy passed with `-D warnings`. The baseline,
post-retirement, and Clippy logs are in `/tmp/forge-post-tool-baseline.log`,
`/tmp/forge-post-tool-rust-post-retirement.log`, and
`/tmp/forge-post-tool-clippy-post-retirement.log`.
