# `forge ledger`: the cost ledger's reader

Step 1 of `docs/design/cost_ledger.md`'s build plan (section 6). This is a
stub: the CLI synopsis for what step 1 ships. Step 6 fills in the rest of
this page once rollups (step 2), calibration (step 3), the `agent_rollup`
join (step 4) and the gate phase (step 5) exist.

## `forge ledger read`

```
forge ledger read <path>... [--json | --summary] [--kind transcript|telemetry]
```

Parses one or more JSONL files -- harness transcripts or hook telemetry --
into a summary, streaming with `BufRead::lines` so a 100 MB transcript never
loads whole into memory. Never panics on a malformed or truncated line; such
lines are counted and reported instead (`skipped_lines`, with line numbers).

- `--summary` (default): one `key=value` line per file.
- `--json`: one `TranscriptSummary`/`TelemetrySummary` JSON object per file
  (JSONL on stdout), keyed to match the design's `turn_attribution` names —
  the input step 2's rollups consume.
- `--kind`: force the input kind instead of detecting it from the first
  parseable line (a transcript line always carries `uuid`; telemetry lines
  carry a top-level `event` and no `uuid`).

Per-block token estimates (`estimated_tokens_by_block_type` in the design)
are not emitted by this step: section 3.4 requires the ledger to refuse an
uncalibrated estimate rather than print one silently, and calibration is
step 3.
