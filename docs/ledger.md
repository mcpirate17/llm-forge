# `forge ledger`: the cost ledger's reader and rollups

Steps 1 and 2 of `docs/design/cost_ledger.md`'s build plan (section 6). This
is a stub: the CLI synopsis for what steps 1-2 ship. Step 6 fills in the
rest of this page once calibration (step 3), the `agent_rollup` join
(step 4) and the gate phase (step 5) exist.

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
step 3. Step 2 (`forge ledger rollup`, below) does compute one -- named
uncalibrated in every row via `estimate_method`, which is what "not
silently" actually forbids; a labeled estimate is not a silent one.

## `forge ledger rollup`

```
forge ledger rollup <path>... [--out <ledger_root>] [--dry-run]
```

Turns transcript/telemetry summaries (the `read` step above) into the three
derived tables step 2 owns: `turn_attribution`, `session_rollup`,
`hook_rollup` (`agent_rollup`, step 4, needs the `Agent:` trailer join and
is out of scope here). A path may be a single file or a directory, walked
non-recursively for its immediate `*.jsonl` children -- a subagent
transcript (`agent-*.jsonl`) sits beside its parent's file and rolls up as
its own session, no special case needed.

- `--out <ledger_root>`: where day files land; defaults to `$LEDGER_ROOT`,
  else `/mnt/data/llm/ledger/`.
- `--dry-run`: print the rows to stdout as JSONL (turn rows, then session
  rows, then hook rows) and write nothing.

Storage: `<ledger_root>/<table>/<utc-date>.jsonl`, one JSON object per line.
Idempotent per session per day: rerunning replaces that session's
(`turn_attribution`/`session_rollup`) or hook's (`hook_rollup`) rows for
that day file rather than duplicating them -- every other row in the file
is left untouched.

### `turn_attribution` fields

`session_id`, `turn_index`, `turn_uuid`, `timestamp`, `model`,
`input_tokens`, `output_tokens`, `cache_read_input_tokens`,
`cache_creation_input_tokens`, `bytes_by_block_type` (the reader's own
per-turn counts), `estimated_tokens_by_block_type` (this turn's billed
input tokens redistributed across blocks by byte share, `thinking`
excluded per section 3), `estimate_method` (always
`byte_proportional_uncalibrated_cpt4` at this step -- the byte-to-token
constant used only where a token count must convert back to an
approximate byte count, e.g. `resend_bytes` below; the ratio split itself
does not need it).

### `session_rollup` fields

`session_id`, `project` (the transcript file's parent directory name),
`first_ts`, `last_ts`, `n_turns`, `n_compactions`, `total_input`,
`total_output`, `total_cache_read`, `total_cache_creation`,
`resend_bytes`, `resend_events`.

Compaction detection prefers the harness's `isCompactSummary` marker line
(one per compaction, unambiguous); the `cache_read_input_tokens` sharp-drop
heuristic only applies to a session with zero markers, so the two signals
are never summed for the same event. Resend detection: a turn whose own
`bytes_by_block_type` total does not exceed the previous turn's, yet still
pays `cache_creation_input_tokens > 0`, is treated as re-caching bytes
already seen -- an approximation (the reader keeps no bytes to prove
identity), not a measurement.

### `hook_rollup` fields

`hook_name`, `event`, `n_calls`, `p50_ms`, `p90_ms`, `total_output_bytes`
(the reader's own per-hook aggregate, one row per hook name per telemetry
file). Filed under the day the rollup command ran, not a per-call
timestamp -- `HookStats` carries none to derive one from.
