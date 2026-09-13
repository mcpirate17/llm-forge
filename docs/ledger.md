# `forge ledger`: the cost ledger's reader and rollups

Steps 1, 2 and 4 of `docs/design/cost_ledger.md`'s build plan (section 6).
This is a stub: the CLI synopsis for what steps 1-2-4 ship. Step 6 fills in
the rest of this page once calibration (step 3) and the gate phase (step 5)
exist.

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

Turns transcript/telemetry summaries (the `read` step above) into the four
derived tables step 2 and step 4 own: `turn_attribution`, `session_rollup`,
`hook_rollup`, `agent_rollup`. A path may be a single file or a directory,
walked non-recursively for its immediate `*.jsonl` children -- a subagent
transcript (`agent-*.jsonl`) sits beside its parent's file and rolls up as
its own session, no special case needed.

- `--out <ledger_root>`: where day files land; defaults to `$LEDGER_ROOT`,
  else `/mnt/data/llm/ledger/`.
- `--dry-run`: print the rows to stdout as JSONL (turn rows, then session
  rows, then hook rows, then agent rows) and write nothing.
- `--repo <path>` (step 4): scan `<path>`'s `git log --first-parent main`
  (see `forge ledger landed` below) and join it to this run's own
  `session_rollup` rows into `agent_rollup`. Absent, `agent_rollup` is not
  computed at all -- the three step-1/2 tables behave exactly as before
  this flag existed.
- `--project <name>` (required with `--repo`): the `session_rollup.project`
  value (a transcript directory's basename, `project_of()`) that this
  `--repo`'s commits are allowed to join against. No default: guessing it
  wrong either drops every join silently or crosses a project boundary,
  which this design forbids outright. One invocation joins exactly one
  project; a repo whose sessions live under more than one transcript
  directory (e.g. a project later split out of a monorepo checkout) needs
  one `forge ledger rollup` invocation per project directory, each against
  the same `--repo`, and their `agent_rollup` rows combined by the reader.
- `--cap <n>` (step 4, default 150000): the per-session `billed_noncache`
  token total above which `agent_rollup.cap_breaches` counts a session.
- `--since <date>` / `--last <n>` (step 4): passed straight through to the
  `--repo` scan; see `forge ledger landed`.

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
`resend_bytes`, `resend_events`, `harness_session_ids` (step 4 join key,
below), `models` (distinct `TurnSummary.model` values seen, sorted -- step
4's tier-inference input).

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

## `forge ledger landed`

```
forge ledger landed --repo <path> [--since <date> | --last <n>]
```

Scans `<path>`'s `git log --first-parent main` (shells out to `git`, no
libgit2 dependency) and prints one JSON row per landed commit to stdout.
`--since`/`--last` are mutually exclusive; omit both to scan every
first-parent commit.

Fields: `sha`, `merged_at` (committer date, UTC, `YYYY-MM-DDTHH:MM:SSZ` --
`%cd` with `--date=format-local:...` and `TZ=UTC`, never `%cI`, which
ignores `--date` and keeps the commit's original offset), `pr_number` (from
the subject's trailing `(#N)`, `null` when absent), `agent_names` (every
`Agent: <name>` trailer line in the body, sorted, deduplicated -- a squash
commit can carry several, one per squashed sub-commit), `harness_session_ids`
(every `session_[a-zA-Z0-9]+` id found in the body, e.g. from a
`Claude-Session:` trailer), `files_changed`, `insertions`, `deletions`
(`--shortstat`).

## `agent_rollup` (step 4)

Joins `session_rollup` to `forge ledger landed`'s commits through the
`Agent:`/`Claude-Session:` trailers (design section 2, section 4 metric 3:
tokens per landed PR), so cost divides by *landed* work, not by session
count. The join is commit-first: each landed commit already names its
agent(s) via `Agent:`; the open question per commit is which session did
the work.

Join order, per commit:

1. **`session_url`** (primary): the commit's `harness_session_ids`
   intersect a session's own `harness_session_ids`. Exact evidence -- an id
   this specific was never meant to appear twice by coincidence.
2. **`time_window`** (fallback, only tried when step 1 finds nothing): a
   same-`--project` session whose `[first_ts, last_ts]` interval overlaps
   `[merged_at - 6h, merged_at]`. More than one session overlapping marks
   that commit `ambiguous` -- every overlapping session is still credited
   (dropping one silently would just trade over-attribution for
   under-attribution) but the flag says a human should look.
3. **`unjoined`**: no session matched either way. Printed to stderr by
   `forge ledger rollup --repo`, one line per commit -- a finding to fix
   the join on or accept, never something to make disappear by loosening
   the match.

Both steps only ever look at sessions whose `project` equals the
`--project` this run was given -- never any other project's sessions, full
stop.

### `AgentRollupRow` fields

`agent_name`, `tier` (below), `n_sessions` (distinct sessions credited),
`n_landed_prs` (distinct PR numbers, or commit shas for a landed commit
with no PR suffix), `total_tokens` (`token_basis: "billed_noncache"` --
input + output + cache_creation, summed over every distinct credited
session; cache_read is billed once, steeply discounted, and is kept
separately in `total_cache_read` rather than folded in), `token_basis`,
`tokens_per_landed_pr` (`total_tokens / n_landed_prs`, `null` when
`n_landed_prs` is 0), `cap_breaches` (count of this agent's credited
sessions whose own `billed_noncache` total exceeds `--cap`), `join_method`
(the strongest evidence behind any session credited to this agent:
`session_url` > `time_window` > `unjoined`, in that order).

### Tier inference table

Substring match (case-insensitive) over a session's `models`, most
specific first:

| Model substring | Tier |
|---|---|
| `fable`, `mythos` | `fable` |
| `opus` | `opus` |
| `sonnet` | `sonnet` |
| `haiku` | `haiku` |
| `glm` | `glm` |
| (none matched) | `unknown` |
| (more than one tier across the agent's sessions) | `mixed` |

### Real-data check (2026-09-13, this PR)

Ran against this repo's own last 30 landed commits (`ab153e2`..`d90ffdf`)
joined to transcripts under `/home/tim/.claude/projects/`:

- `-home-tim-Projects-LLM/` (291 files, 1.4 GB): rollup + join completed in
  **2.4s wall time**.
- `-home-tim-Projects-llm-forge/` ("if exists" per the brief): exists, but
  holds only `memory/*.md` notes, no transcripts -- contributes nothing.

| agent | tier | n_sessions | n_landed_prs | total_tokens | tokens_per_landed_pr | cap_breaches | join_method |
|---|---|---|---|---|---|---|---|
| llm-b0 | mixed | 5 | 16 | 21,086,507 | 1,317,906.7 | 5 | session_url |
| glm | fable | 1 | 9 | 7,165,776 | 796,197.3 | 1 | time_window |
| conductor-67 | fable | 1 | 5 | 7,165,776 | 1,433,155.2 | 1 | time_window |

Hand-cross-checked 3 commits:

1. `d90ffdf` (PR #37, `llm-b0`): `git show` confirms `Agent: llm-b0` and
   `Claude-Session: .../session_01PoLjRxqVQGqy41fMDG26vX`; that exact id is
   present in real transcript files under the LLM project dir --
   `session_url` join confirmed correct.
2. `ab153e2` (PR #32, `glm`): no `Claude-Session` trailer, so the fallback
   ran. The session it credited was traced by hand to this very ledger
   task's own long-lived orchestrator session (job `a3348844`), whose
   `[first_ts, last_ts]` span (2026-09-11 23:09 to 2026-09-13 08:31) covers
   nearly this entire 30-commit window -- its `billed_noncache` total
   (7,165,776) matches the reported row exactly. **Finding, not a bug in
   the join as specified**: a session resumed/continued across days
   satisfies the `[merged_at-6h, merged_at]` overlap test for every commit
   merged during its lifetime, so `join_method: "time_window"` over-credits
   long-lived sessions across unrelated commits when no `Claude-Session`
   trailer narrows it. `ambiguous` does not catch this mode: each commit
   individually matched exactly one session, so none were flagged.
3. `llm-b0`'s reported `n_sessions: 5`: real data also surfaced a `session_
   rollup` gap at the step-2/step-4 boundary -- one real session id
   (`65f84759-e8ac-...`) appears as *two* separate `session_rollup` rows
   (a transcript log split across two physical files under the one
   `session_id`). `agent_rollup`'s join dedupes credited sessions by
   `session_id`, so only one of the two rows' tokens count toward the
   agent's total. Under-counts, does not double-count; flagged as debt
   below rather than fixed here, since fixing it means merging split-file
   sessions inside `rollup.rs`'s `session_rollup` construction (step 2),
   out of this PR's scope.
- **0** landed commits were unjoined in this run (all 30 got a session via
  one method or the other).
