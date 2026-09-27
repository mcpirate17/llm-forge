# Memory sidecar and streaming test migration

The two retired Python modules contained nine declared tests and no parametrized
decorators. Their assertions and fixtures now live in two Rust integration test
targets under `native/conductor-native/tests/`. Each case calls the existing
Python API through PyO3; no pytest module is invoked by the Rust tests. The
shared Rust fixture creates local JSONL files, deterministic vectors, and a
fixed embedder callback. It does not contact an embedding or model service.

| Retired Python module and test | Rust target and test | Preserved cases and assertions |
| --- | --- | --- |
| `test_memory_vectors.py::test_sidecar_builds_once_and_reloads_from_mmap` | `python_contracts_memory_vectors.rs::sidecar_builds_once_and_reloads_from_mmap` | Sidecar files exist; `(4, 3)` float32 matrix; row title and vector removal; metadata mtime stays fixed; second matrix is a memmap and retains four rows. |
| `test_memory_vectors.py::test_sidecar_rebuilds_when_index_changes` | `python_contracts_memory_vectors.rs::sidecar_rebuilds_when_index_changes_and_metadata_is_corrupt` | Appending a newline changes the metadata payload; corrupt JSON metadata triggers a four-row rebuild. |
| `test_memory_vectors.py::test_search_applies_recency_and_dedup` | `python_contracts_memory_vectors.rs::search_applies_recency_and_dedup` | Week-old near match outranks old exact match; near duplicate collapses; exact score and weighted ordering; `boost=0.0` and `dedup_cosine=1.01` restore the unweighted three-title order. |
| `test_memory_vectors.py::test_search_validates_inputs` | `python_contracts_memory_vectors.rs::search_validates_top_k_dimension_and_row_count` | Three distinct `ValueError` cases: `top_k=0`, two-dimensional query for three-dimensional index, and one row against a four-row matrix. |
| `test_memory_vectors.py::test_recency_weights_bounds` | `python_contracts_memory_vectors.rs::recency_weights_bounds_and_missing_file` | `boost=0.5`, `half_life_days=30`; missing file has weight 1; old and new files satisfy `1 < old < new <= 1.5`. |
| `test_memory_index_native_streaming.py::test_query_index_file_streams_with_stable_exact_contract` | `python_contracts_memory_index_streaming.rs::query_index_file_streams_with_stable_exact_contract` | Trimmed instructed embed call; stable order for two equal scores; scores 1; 501 Unicode characters truncate to 500; exact five result keys; one-dimensional embedder raises `RetrieveError`. |
| `test_memory_index_native_streaming.py::test_first_query_builds_the_sidecar_and_second_query_reuses_it` | `python_contracts_memory_index_streaming.rs::first_query_builds_the_sidecar_and_second_query_reuses_it` | Three-row sidecar created; exactly one build message; second query emits no build message and returns equal results. |
| `test_memory_index_native_streaming.py::test_appending_a_row_rebuilds_the_sidecar_and_ranks_it` | `python_contracts_memory_index_streaming.rs::appending_a_row_rebuilds_the_sidecar_and_ranks_it` | Initially orthogonal rows score zero; appended aligned row causes exactly one four-row rebuild, wins with score 1 and original path. |
| `test_memory_index_native_streaming.py::test_sidecar_query_matches_the_full_scan_reference` | `python_contracts_memory_index_streaming.rs::sidecar_query_matches_the_full_scan_reference` | 120 deterministic four-dimensional LCG rows; reference scan selects the winner before its text is lengthened to 600 Unicode characters; sidecar and scan top ten paths agree, score deltas stay below `1e-5`, and winner text truncates to 500. |

The vector fixture preserves the original four title/vector combinations and
old/new file ages. It replaces only `memory_index.load_index` with a Rust
callback returning Rust-built data. The callbacks preserve the original argument
counts and parameter names, and streaming rows read the current schema constant.
Streaming cases supply an injected callback and
exercise the existing native sidecar through `memory_index.query_index_file`;
the full-scan comparison calls `_query_index_file_scan` directly.

A repository-wide name and helper search found no import consumers of either
retired test module or its private helpers. The only other path references are
the historical inventory in `docs/native-agent-context-tests-migration.md`.
`src/conductor/conftest.py` and all shared fixture data remain in place.

Before retirement, the two Python targets passed all nine tests. After the
port, the Rust targets passed five vector and four streaming tests with
`python-compat-tests`, the pinned Rust toolchain, offline Cargo, and one test
thread. The run masked CUDA and limited CPU thread pools; `FORGE_BIN` pointed
at the existing release executable. Scoped Clippy with warnings denied and
exact-file rustfmt were also run. `gh issue list --limit 30` showed no open
issue covering this migration.
