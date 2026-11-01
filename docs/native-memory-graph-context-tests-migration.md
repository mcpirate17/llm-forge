# Historical memory, notes, and graph-context test migration

This page preserves the original Python-to-Rust test mapping. The
`conductor.index_notes` adapter and `python_contracts_index_notes` target were
retired after native parity passed. Other rows document separate cohorts; their
target status is not implied here. Historical results below are not a current
test inventory.

| Historical Python test module | Rust target at time of migration | Historical coverage |
|---|---|---|
| `test_memory_auto_index.py` (6) | `python_contracts_memory_auto_index` (6) | Indexed-path filter; coalesced markers and receipt; concurrent single writer; marker arrival during refresh; fail-closed marker retention; clean session-end scan. |
| `test_index_notes.py` (2 functions, 4 pytest invocations) | `python_contracts_index_notes` (2 tests, 4 fixture runs; now retired) | FTS rows, titles, bodies, source, paths, table content, excluded audit paths, and ordered search parity against the native notes CLI. |
| `test_graph_context.py` (18) | `python_contracts_graph_context` (18) | AST skeleton forms; graph callers/callees and target symbols; Markdown role binning; syntax-only caller scan; errors, CLI output, and test-path classification. |

## Focused notes results

Before removing the compatibility target, `python_contracts_index_notes`
passed 2 tests across 4 fixture runs against a freshly built native CLI. The
native notes suite passed 17 unit tests and 14 CLI tests. These are focused
notes-index results; they are not a full repository gate.

## Historical cohort record

The original source revisions were SHA-256
`9fbaef4e0600c5762922bfa8d526b6ed95fc339c1d6c859210fc26ba60810bb0`,
`2db66b1f258ba1c7f37b46685db7f1cc0f9bd1f93cd4a0d6097dbe60ae63331e`,
and `d3bebf2d97f6a4f7f7dc38bb22b3a9603f6ea1aa4dd4d37539602b4e7d10d29d`.
Before retirement, the old cohort reported 28 expanded pytest cases passing.
Its notes fixture used the prebuilt Forge `0.8.0 (git a755941)` binary with
SHA-256 `122db29fccfc5f1e0ce16189b0fd4ff893735a3da857f461c3f7209ebb09ddf8`.
The historical notes compatibility run used 2 Rust tests with four fixture
runs; that target is no longer present.

Other old cohort results, including the 26-case report and scoped Clippy
result, describe that earlier state only. The old multi-target Cargo command
was removed because it names retired compatibility targets. Current native
memory and graph behavior is documented in
[`native-memory-ingestion.md`](native-memory-ingestion.md); the notes command
contract is in [`native-notes-index.md`](native-notes-index.md).
