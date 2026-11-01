# Native vector index

`conductor_native::vector_index` provides native cosine search, HNSW construction,
and immutable mmap snapshots under the `vector-search` feature. No Python runtime
or model server is needed to open or search an index. Embedding generation and
code/BM25 snapshot integration are separate interfaces under development.

`Space` binds dimension and a nonzero model SHA-256. `Query` checks that identity
and normalizes once; a different model or dimension fails before scoring.
Construction consumes a contiguous f32 matrix and normalizes with f64 accumulation.
Zero, nonfinite, malformed and oversized vectors fail explicitly. Limits are one
million rows, 8,192 dimensions, 4 GiB of vectors, M=4..64 and ef=1..4,096.

HNSW uses deterministic seeded geometric levels, layer search, reciprocal edge
insertion and diversity pruning with retained pruned candidates. The algorithm
follows [Malkov and Yashunin](https://arxiv.org/abs/1603.09320). Exact search remains
an independent candidate-selection path for correctness, recall measurement and
workloads where scanning is faster. Both paths rank by cosine then original row
ordinal. SIMD uses runtime-detected AVX2/FMA with f64 lanes and a portable f64
fallback. Summation order can differ across backends; reproducibility of the
serialized graph is scoped to the same backend and ordered input.

`QueryWorkspace` reserves the visit stamps, frontier, result heap and output once.
The frontier is bounded by the row count, not by assumptions about graph quality.
Successful prepared queries, exact scans and individual reranks allocate zero
times. Workspaces belong to individual concurrent readers. Query preparation,
construction, snapshot verification and result serialization are separate costs.

The v2 file contains an aligned f32 matrix, offset table, packed graph, model and
source-set hashes, header checksum and full payload checksum. Opening verifies
shapes, node offsets, edge identities/layers/degrees, normalization and checksums.
Readers pin a read-only inode; atomic replacement and unlink leave existing
readers valid. Publication must never modify or truncate a mapped inode. The
same-user owner is trusted; hashes are corruption checks, not authentication.
Symlinks, hard links, writable files and foreign owners are rejected. The mapped
format currently requires a little-endian host. An operator must supply truthful
model/source identities; these hashes do not prove how embeddings were generated.

## Validation and measurement

The release tests cover unseen-query recall, exact-score/tie controls, invalid
vectors/models, malformed graph bounds with recomputed checksums, corruption,
immutable publication, concurrent pinned readers, SIMD tails/subnormals and
allocation counting. At 4,096 rows × 64 dimensions and ef=160, recall@10 was
0.996 for uniform vectors and 1.000 for a 16-cluster fixture (100 unseen queries).

Larger matched CPU benchmark on 2026-10-31, rustc 1.98.0, Ryzen 9 9950X3D,
shared Linux host: 32,768 rows × 384 dimensions, 100 unseen queries, seed 721716,
M=16, construction ef=200. Two warmup batches preceded ten measured batches.
Values below are median batch means, not individual-call tail latency. Fixtures
are deterministic synthetic vectors, not evidence of semantic code relevance.

| Fixture | Search | ef | Recall@10 | Median query |
|---|---|---:|---:|---:|
| Uniform | Exact | — | 1.000 | 924.7 µs |
| Uniform | HNSW | 64 | 0.199 | 246.4 µs |
| Uniform | HNSW | 160 | 0.371 | 537.4 µs |
| Uniform | HNSW | 400 | 0.632 | 1,178.3 µs |
| 32 clusters | Exact | — | 1.000 | 948.4 µs |
| 32 clusters | HNSW | 64 | 0.964 | 127.0 µs |
| 32 clusters | HNSW | 160 | 0.990 | 169.5 µs |
| 32 clusters | HNSW | 400 | 0.990 | 208.1 µs |

Building took 14.458 s (uniform) and 3.427 s (clustered). Verified opening took
27 ms each, with warm filesystem cache; this includes checksums and structural
checks, not storage-cold latency. Snapshots occupied 54,313,540 and 54,331,936
bytes. Query scratch reserved 674,560 bytes at ef capacity 400. Process RSS after
each fixture was 61,912 and 62,812 KiB; peak across both fixtures was 114,284 KiB
and includes construction. Embedding generation and filesystem freshness checks
are excluded. HNSW is approximate and can lose whole neighborhoods; operators
must evaluate recall on their own embeddings and retain an exact mode.

The tested benchmark executable SHA-256 was
`e63c0cb4865d9e6cc102da788098278cd21c2b8d8f8af27fe3c7ba6214e7d82f`;
benchmark source SHA-256 was
`83e898c88ddc7ad32646938a0ecf67cc0a80c88a571c7e5f9e73633e22e02440`.
Run with:

```sh
cargo +1.98.0 bench --locked --manifest-path native/conductor-native/Cargo.toml \
  --no-default-features --features vector-search,bundled-sqlite --bench vector_index
```

These observations justify a CPU search path; they do not demonstrate a tensor
bottleneck or warrant launching GPU kernels.
