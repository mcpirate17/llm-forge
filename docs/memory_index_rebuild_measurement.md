# Where the 5-minute `memory_index index` goes (measured, not assumed)

Whole-catalog `python -m conductor.memory_index index` on the LLM host
takes over 5 minutes, so callers with a 300 s timeout kill it (exit 143,
read as a failure). Before porting any of it to Rust we measured where
the wall time actually goes. Verdict up front: **no port needed yet**.
The unchanged run is 3.9 s; the minutes are embed of accumulated real
change, amplified by file-level reuse granularity on two append-only
journals. Nothing in the incremental path is broken.

## Method

- Scratch host at `~/.cache/llm-forge/scratch/memidx-host/`: a copy of
  LLM's `conductor/` package, `memory_sources.toml`, every source tree
  it names, and the 406 MB `research/cache/memory_index.jsonl`. The
  JSONL's reuse keys are resolved absolute paths, so the copy rewrote
  the `/home/tim/Projects/LLM` prefix to the host path (8,915 lines;
  absolute-root sources -- CodexVault, `~/.codex/memories`,
  `~/.claude/tasks` -- were untouched and stay valid on the same
  machine). `conductor.memory_index.ROOT` was verified to resolve to
  the copy; imports via LLM's venv, no installs.
- Embedding route (from the index's own meta rows): local-qwen3 ->
  ollama at `127.0.0.1:11434` (7317 is the broker), model
  `qwen3-embed-cpu`, dimension 1024, `num_gpu: 0`, `num_ctx` 2048,
  `max_input_chars` 2000 -- the CPU route, on purpose
  (`CPU_EMBED_NUM_GPU=0` in every run).
- Driver script (below) mirrors the CLI index path exactly
  (`index_write_lock` -> `build_index_result` -> `save_index_result`
  when changed) with `time.perf_counter()` wraps on the four phases and
  an `embed_batch` call counter. `/usr/bin/time -v` for wall and RSS.
- The first run's index snapshot was ~40 min stale against the copied
  trees (LLM last indexed at 19:10; files kept changing after). That
  staleness is itself the finding: it is exactly what a real caller
  sees when indexing infrequently. Run (d1) reproduces (a1) with
  per-path instrumentation to attribute the fresh chunks.

## Numbers

| run | input | wall s | parse s | collect s | embed s | write s | reused / embedded | peak RSS |
|-----|-------|--------|---------|-----------|---------|---------|-------------------|----------|
| a1 | whole catalog, pristine copy | 593.7 | 3.8 | 0.3 | 585.2 | 4.4 | 15738 / 972 | 844 MB |
| a2 | whole catalog, unchanged rerun | 3.9 | 3.6 | 0.3 | -- | -- | 16710 / 0 | ~840 MB |
| b1 | `--sources notes`, unchanged | 3.5 | 3.4 | 0.1 | -- | -- | 5251 / 0 (+11459 preserved) | -- |
| b2 | `--sources notes`, rerun | 3.6 | 3.5 | 0.1 | -- | -- | 5251 / 0 (+11459 preserved) | -- |
| c1 | one note +1 line, whole catalog | 12.4 | 3.7 | 0.3 | 4.1 | 4.3 | 16699 / 11 | -- |
| d1 | a1 reproduced, instrumented | 572.5 | 3.5 | 0.3 | 564.4 | 4.3 | 15727 / 983 | 844 MB |
| probe | `embed_batch` of 32 tiny texts | 1.23 | | | | | 39 ms/text | |

(`--sources notes` "finishes in about a minute" on the host but 3.5 s
here for the same reason a1 takes minutes: when nothing changed there
is nothing to embed. A notes run that touches a few changed notes pays
the same ~0.6 s per fresh chunk -- a minute on the host is ~100 fresh
note chunks. The 3.4 s parse is paid either way: the partial run still
scans the whole JSONL; out-of-scope rows are simply preserved rather
than rewritten.)

### Where a1/d1's ~980 fresh chunks came from (d1, per-path)

| source | fresh chunks | distinct files | dominated by |
|--------|--------------|----------------|--------------|
| codex-memories | 549 | 1 | `~/.codex/memories/raw_memories.md` (648 KB, append-only) |
| vault-unique | 414 | ~23 | `CodexVault/claude/2026-09-13.md` daily note (490 KB) + `2026-09-12.md` (50 KB) + ~20 changed memory notes + 2 regenerated dashboards |
| notes | 18 | 4 | genuine edits (incl. the probe line this measurement appended to one note) |
| cards / repo-tasks | 1 + 1 | 2 | genuine edits |

Two append-only journals alone are ~960 of 983 fresh chunks.

## Decision

Closest to (iv), with the amplifier named: **the wall is honest**.

- No reuse bug: the unchanged whole-catalog run reuses 16710/16710
  rows in 3.9 s with zero embeds and no rewrite (`changed=False`
  skips the JSONL write entirely). `_reuse_rows` does what it
  promises.
- Embed owns the wall only when content genuinely changed: 585 s of
  594 s in a1, 564 s of 572 s in d1 (~0.6 s per 2000-char chunk on
  the CPU route; the 32-text probe shows per-text cost scales with
  tokens, not a batching bug). An 11-chunk delta (c1) costs 12.4 s
  end to end.
- The rewrite is not the wall (4.3 s, and skipped when unchanged);
  the parse floor is 3.4-3.8 s per whole-catalog run, every run.
- The amplification: reuse keys on the whole-file sha256, so one
  appended line to `raw_memories.md` re-embeds all 549 of its chunks
  (~5.5 min on the CPU route), and one line to a 490 KB daily note
  re-embeds ~250. The 300 s caller timeout is beaten by ~500+
  accumulated changed chunks, which is what "index weekly" produces.

So: port nothing yet. The levers, in order of leverage:

1. **Index more often.** Each run pays only the delta since the last;
   at daily cadence the wall is seconds. This is operational, free.
2. **Chunk-hash reuse for append-only files** (match a fresh chunk to
   any previous row of the same path on chunk-text hash even when the
   file sha differs). Collapses a `raw_memories.md` append from ~549
   chunks to ~1. A deliberate design change with ordering subtleties,
   not a 40-line bug fix -- needs its own slice if wanted.
3. **The embed route** is the per-chunk cost lever (~0.6 s/chunk CPU
   vs whatever the GPU route gives); orthogonal to indexing.
4. **The caller timeout** (300 s, exit 143 read as failure) should
   either rise or the index should run outside the timed path; at
   current cadence any whole-catalog run that touches a journal will
   exceed it honestly.

Residual honesty notes: a1/d1 `removed=1` (one JSONL row with no
emitted counterpart -- snapshot drift artifact, not data loss); c1's
probe line was appended to
`research/notes/cdma_topk_dim96_20k_diagnosis_2026-07-26.md` in the
scratch copy only (LLM's tree untouched); the scratch host was deleted
after measurement.

## Driver

```python
"""Phase-split driver for `memory_index index` (slice Z2 measurement).

Mirrors the CLI's index path exactly (index_write_lock + build_index_result +
save_index_result when changed) while timing each phase and counting embed
HTTP batches. Usage: python measure_driver.py [source_ids|none-for-all]
"""

from __future__ import annotations

import json
import sys
import time

import conductor.memory_index as mi

PHASES: dict[str, float] = {}
EMBED = {"batches": 0, "texts": 0}


def timed(name, fn):
    def wrapper(*args, **kwargs):
        start = time.perf_counter()
        try:
            return fn(*args, **kwargs)
        finally:
            PHASES[name] = PHASES.get(name, 0.0) + time.perf_counter() - start

    return wrapper


def counted_batch(texts, **kwargs):
    EMBED["batches"] += 1
    EMBED["texts"] += len(texts)
    return mi.__dict__["_real_embed_batch"](texts, **kwargs)


def main() -> int:
    source_ids = None
    if len(sys.argv) > 1 and sys.argv[1] != "none":
        source_ids = {part.strip() for part in sys.argv[1].split(",") if part.strip()}

    mi._load_previous_rows = timed("parse_previous_jsonl", mi._load_previous_rows)
    mi._collect_index_chunks = timed("collect_hash_chunk_sources", mi._collect_index_chunks)
    mi._embed_fresh_chunks = timed("embed_fresh_http", mi._embed_fresh_chunks)
    mi.save_index_result = timed("write_jsonl", mi.save_index_result)
    mi.__dict__["_real_embed_batch"] = mi.embed_batch
    mi.embed_batch = counted_batch

    real_collect = mi._collect_index_chunks

    def collect_with_paths(entries, selected_ids, previous):
        fresh, reused, emitted = real_collect(entries, selected_ids, previous)
        by_source: dict[str, int] = {}
        for row in fresh:
            by_source[row["source"]] = by_source.get(row["source"], 0) + 1
        print("FRESH_BY_SOURCE " + json.dumps(by_source), flush=True)
        paths = sorted({r["path"] for r in fresh})
        print(f"FRESH_PATHS {len(paths)} " + json.dumps(paths[:40]), flush=True)
        return fresh, reused, emitted

    mi._collect_index_chunks = collect_with_paths

    t0 = time.perf_counter()
    with mi.index_write_lock():
        result = mi.build_index_result(source_ids=source_ids)
        path = mi.INDEX_PATH
        if result.changed or not path.is_file():
            path = mi.save_index_result(result)
    total = time.perf_counter() - t0

    print(
        json.dumps(
            {
                "total_s": round(total, 2),
                "phases_s": {k: round(v, 2) for k, v in PHASES.items()},
                "unaccounted_s": round(total - sum(PHASES.values()), 2),
                "embed": EMBED,
                "result": {
                    "rows": result.total_rows,
                    "reused": result.reused_count,
                    "embedded": result.embedded_count,
                    "preserved": result.preserved_count,
                    "removed": result.removed_count,
                    "changed": result.changed,
                },
                "wrote_jsonl": "write_jsonl" in PHASES,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```
