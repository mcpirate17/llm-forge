# Memory auto-index, notes-index, and graph-context test migration

Three Python pytest modules were retired after their cases moved to Rust-owned
PyO3 contract targets. The tests call the live `conductor` production modules;
they do not import or execute the retired pytest modules. All assertions,
fixture inputs, callback behavior, SQLite comparisons, and CLI output checks
are owned by Rust. No Python production algorithm changed.

| Retired Python module and cases | Rust contract target | Preserved coverage |
|---|---|---|
| `test_memory_auto_index.py` (6) | `python_contracts_memory_auto_index.rs` (6) | Indexed-path filter; coalesced markers and receipt; concurrent single writer; marker arrival during refresh; fail-closed marker retention; clean session-end scan. Rust callbacks retain the original runner count and selected-source assertions. |
| `test_index_notes.py` (2 functions, 4 pytest invocations) | `python_contracts_index_notes.rs` (2 tests, 4 fixture runs) | Python vs Forge FTS rows, titles, bodies, source, paths, float mtime tolerance, table content, excluded audit paths, and nonempty ordered search-path parity for `apple`, `banana`, and `Titled Heading`. Each search row gets a fresh fixture and two fresh databases. |
| `test_graph_context.py` (18) | `python_contracts_graph_context.rs` (18) | AST functions/classes/filter/async/assignment forms; graph callers/callees and target symbol; full file context; Markdown role binning; syntactic caller scan; missing-file, syntax, unknown-symbol and CLI errors; JSON CLI output; frozen relationship; six test-path classifier rows. The repository fixture still initializes and commits a Git repository, and SQLite uses the original `nodes`/`edges` schema. |

The original source revisions were SHA-256
`9fbaef4e0600c5762922bfa8d526b6ed95fc339c1d6c859210fc26ba60810bb0`,
`2db66b1f258ba1c7f37b46685db7f1cc0f9bd1f93cd4a0d6097dbe60ae63331e`,
and `d3bebf2d97f6a4f7f7dc38bb22b3a9603f6ea1aa4dd4d37539602b4e7d10d29d`,
respectively. Before retirement, all 28 expanded pytest cases passed in
`/tmp/forge-cohort-a-python-baseline.log`. The original index-notes fixture's
binary selector alone was overridden in process so it selected the prebuilt
release Forge `0.8.0 (git a755941)`, SHA-256
`122db29fccfc5f1e0ce16189b0fd4ff893735a3da857f461c3f7209ebb09ddf8`,
instead of rebuilding a mutable debug binary. Original test bodies and the
three parameter rows were unchanged.

Native validation supplies `FORGE_BIN` from the environment, with no local
binary path tracked in source. The focused command is:

```sh
PYO3_CONFIG_FILE=/tmp/forge-platform-pyo3-config.txt \
SQLITE3_LIB_DIR=/tmp/forge-sqlite-link \
PYTHONPATH=/tmp/forge-contract-native46:$PWD/src:$PWD/.venv/lib/python3.12/site-packages \
FORGE_BIN=/tmp/forge-native-baselines/a755941/forge \
CARGO_BUILD_JOBS=2 CUDA_VISIBLE_DEVICES='' ROCR_VISIBLE_DEVICES='' \
cargo +1.98.0 test --offline --locked \
  --manifest-path native/conductor-native/Cargo.toml \
  --features python-compat-tests -j 2 \
  --test python_contracts_memory_auto_index \
  --test python_contracts_index_notes \
  --test python_contracts_graph_context -- --test-threads=1
```

The final review retained Python list/set equality and callback parameter binding,
canonicalized graph fixture paths, and restored the 60-second notes CLI timeout.
CLI output is captured in fixture-owned files so a full pipe cannot block the
deadline. An isolated Rust fake executable wrote 8 MiB and then stalled; the
contract failed at 60.04 seconds as expected, and its child was killed and reaped.
All 26 Rust cases and scoped Clippy passed after the final repairs.

Before removing the exact Python paths, a repository-wide reference audit
found no active Python imports or direct test selectors for these modules.
`src/conductor/conftest.py` remains for other pytest cohorts. Historical
references remain in `docs/roadmap.md`, `docs/native-graph-context-migration.md`,
`docs/native-agent-context-tests-migration.md`, an archived graph-context
campaign, the duplication baseline, and grandfathered candidate-review node
IDs. They are retained as historical data; those documents' descriptions of
the old pytest location are now stale and need a separate documentation pass.
