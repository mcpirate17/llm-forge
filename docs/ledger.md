# `forge ledger`: the cost ledger's reader and rollups

Steps 1, 2, 4 and 5 of `docs/design/cost_ledger.md`'s build plan (section 6).
This is a stub: the CLI synopsis for what steps 1-2-4-5 ship. Step 6 fills in
the rest of this page once calibration (step 3) exists.

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

   Two restrictions keep this fallback from over-attributing, both found
   via the real-data check below and since closed:

   - **Fallback-eligible sessions must carry no `harness_session_ids` of
     their own.** A session that ever saw its own claude.ai URL would have
     joined by `session_url` on its own commits; letting it also catch a
     different, URL-less commit via time_window is exactly how a
     long-lived coordinator session ended up credited with unrelated PRs
     (finding 2 below). Such sessions are excluded from the fallback pool
     entirely, not merely scored lower.
   - **A `glm`-named agent's commit only falls back to a session whose own
     `models` mention `glm`.** Crediting a `glm` commit to whichever
     unrelated session merely happened to overlap in time is worse than
     reporting it unjoined.
3. **`unjoined`**: no session matched either way -- including a commit that
   had fallback candidates but none passed the restrictions above. Printed
   to stderr by `forge ledger rollup --repo`, one line per commit -- a
   finding to fix the join on or accept, never something to make disappear
   by loosening the match.

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
  **2.2s wall time**.
- `-home-tim-Projects-llm-forge/` ("if exists" per the brief): exists, but
  holds only `memory/*.md` notes, no transcripts -- contributes nothing.

Hand-cross-checked 3 commits:

1. `d90ffdf` (PR #37, `llm-b0`): `git show` confirms `Agent: llm-b0` and
   `Claude-Session: .../session_01PoLjRxqVQGqy41fMDG26vX`; that exact id is
   present in real transcript files under the LLM project dir --
   `session_url` join confirmed correct.
2. `ab153e2` (PR #32, `glm`): no `Claude-Session` trailer, so the fallback
   ran. In the first pass (before the fix below) the session it credited
   was traced by hand to this very ledger task's own long-lived
   orchestrator session (job `a3348844`), whose `[first_ts, last_ts]` span
   (2026-09-11 23:09 to 2026-09-13 08:31) covers nearly this entire
   30-commit window -- its `billed_noncache` total (7,165,776) matched the
   reported row exactly. **Finding**: a session resumed/continued across
   days satisfies the `[merged_at-6h, merged_at]` overlap test for every
   commit merged during its lifetime, so `time_window` over-credited
   long-lived sessions across unrelated commits when no `Claude-Session`
   trailer narrowed it -- and `ambiguous` did not catch it, since each
   commit individually matched exactly one session. **Fixed** (see join
   order above): a session that carries its own `harness_session_ids` is no
   longer fallback-eligible at all, and a `glm`-named commit's fallback is
   additionally restricted to sessions whose `models` mention `glm`. Since
   the real transcripts under this project contain no logged GLM sessions
   at all, both `glm` and `conductor-67` are now honestly `unjoined` rather
   than mis-credited (see the updated table).
3. `llm-b0`'s reported `n_sessions: 5`: real data also surfaced a `session_
   rollup` gap at the step-2/step-4 boundary -- one real session id
   (`65f84759-e8ac-...`) appears as *two* separate `session_rollup` rows
   (a transcript log split across two physical files under the one
   `session_id`). `agent_rollup`'s join dedupes credited sessions by
   `session_id`, so only one of the two rows' tokens count toward the
   agent's total. Under-counts, does not double-count; flagged as debt
   below rather than fixed here, since fixing it means merging split-file
   sessions inside `rollup.rs`'s `session_rollup` construction (step 2),
   out of this PR's scope. Still true after the time_window fix above,
   unaffected by it.

Table after the fix:

| agent | tier | n_sessions | n_landed_prs | total_tokens | tokens_per_landed_pr | cap_breaches | join_method |
|---|---|---|---|---|---|---|---|
| llm-b0 | mixed | 5 | 16 | 21,101,155 | 1,318,822.2 | 5 | session_url |
| glm | unknown | 0 | 9 | 0 | 0.0 | 0 | unjoined |
| conductor-67 | unknown | 0 | 5 | 0 | 0.0 | 0 | unjoined |

**14 of 30** landed commits are now `unjoined` (up from 0 before the fix):
every `glm`/`conductor-67` commit that previously borrowed the coordinator
session's tokens. This is the honest result, not a regression to fix --
those agents' actual work is not logged as a harness transcript under this
project at all (GLM sessions run outside this harness), so `0` credited
tokens is correct; the alternative was crediting someone else's session,
which is what this fix removes. Closing this gap for real needs those
agents' own token accounting to land in a transcript this ledger can see,
which is out of this PR's scope.

## `forge ledger audit` (step 5)

```
forge ledger audit --ledger-root <dir> --baseline <file> [--window-days N] [--record]
```

Computes the three budget-ratchet metrics from design section 4 over a
trailing `N`-day window ending today (`--window-days`, default 7) and either
records them as the new baseline (`--record`) or checks them against the
last recorded one. Never rounds a tie up to `PASS`; never invents a value
for a metric with no rows in the window.

- **`median_hook_ms`**: the weighted median of `hook_rollup.p50_ms` across
  the window's day files, weighted by each row's `n_calls` (a hook called
  1,000 times outweighs one called once, even at the same `p50_ms`).
- **`resend_bytes_per_session`**: the mean of `session_rollup.resend_bytes`
  across the window's sessions.
- **`tokens_per_landed_pr`**: `sum(total_tokens) / sum(n_landed_prs)` over
  `agent_rollup` rows whose `join_method` is not `unjoined` -- an
  unattributed agent's tokens have no landed-PR denominator to divide by
  honestly, so they are excluded rather than either dropped silently from
  the numerator alone or credited to `n=0`.

Per-metric status, `value` compared against the recorded `baseline.value`
with `baseline.tolerance_pct` (5% at record time, from `DEFAULT_TOLERANCE_PCT`,
not reconfigurable per metric today):

| Status | Meaning |
|---|---|
| `PASS` | `value` strictly improved on the baseline (`value < baseline`). |
| `RATCHET_HELD` | `value` is within tolerance of the baseline but did not improve on it -- a tie is `RATCHET_HELD`, never rounded up to `PASS`; this is what the design's own worked example computes on a fresh baseline checked against itself. |
| `REGRESSION` | `value` exceeds `baseline * (1 + tolerance_pct / 100)`. |
| `NO_BASELINE` | the metric has a value this window but no recorded baseline entry to compare against (never recorded, or explicitly skipped at record time for lack of data). |
| `NO_DATA` | the metric has no rows in this window at all (`value` is `null`), regardless of what the baseline says. |

Overall `status` is the worst of the three per-metric statuses, ranked
`REGRESSION > NO_DATA > NO_BASELINE > RATCHET_HELD > PASS`. If **every**
metric has zero rows in the window the command fails loud instead of
printing a hollow verdict: it prints an error to stderr and exits `3`
without writing anything, `--record` included -- an empty window is not a
baseline.

`--record` is the only way a baseline file changes; a metric with no data
at record time is omitted from the written baseline (one line to stderr
naming it as debt, not a failure) rather than recorded as `0` or copied
forward from whatever was there before. The baseline file
(`ledger/cost_budget_baseline.json` by convention, tracked in git) also
carries `recorded_utc` and a `ledger_root_sha` -- a `sha256` over every day
file actually read, path and bytes, sorted -- so a baseline receipt names
exactly which rows it was computed from.

### Gate wiring: `cost-budget-audit`

`src/conductor/cost_budget_audit.py` wraps this command as gate phase
`cost-budget-audit` (`conductor.gate.run_gate`): it resolves the `forge`
binary the same way `project_init.py` already does, shells out with the
export root's `ledger/cost_budget_baseline.json` as `--baseline`, and maps
the JSON verdict onto a `PhaseResult` with `ok = False` iff at least one
metric's status is `REGRESSION`. `PASS`, `RATCHET_HELD`, `NO_BASELINE` and
`NO_DATA` -- including the hard-empty exit-3 case where every table is
empty -- are all `ok`: a fresh clone or CI runner has no recorded baseline
and no ledger rows on its first run, and there is nothing to regress
against yet, so that must not make `make gate` permanently red. `detail`
still names every metric's status verbatim, never rounded up to `PASS`, so
`RATCHET_HELD`/`NO_BASELINE`/`NO_DATA` stay visible in gate output even
though they do not fail the phase. Only a missing `forge` binary or output
that fails to parse at all raises loud (`CostBudgetAuditError` ->
`GateRefusal`) -- those are tool failures, not verdicts. The direct CLI path
(`make cost-budget-audit`, `forge ledger audit` run by a human) is
unchanged and still fails loud: exit 3 on the hard-empty window, exit 1 on
anything but `PASS`/`RATCHET_HELD`.

The design also names a `ledger/registry.d/<scope>.json` convention
(mirroring `campaigns/registry.d/`) for concurrency-safe baseline pointers.
That split is coupled to mutation-campaign manifest fragments and a native
registry loader that only understands that one shape (`mutation_registry_
split.py`); it does not generalize to an arbitrary metric baseline, so this
phase uses one tracked file instead, as the design permits when the
registry.d code path does not already generalize.

`make cost-budget-audit` / `make cost-budget-record` (`conductor.mk`) run
this against the live repo (not an export -- there is no candidate review
happening at the command line), mirroring `mutation-patch-audit` /
`mutation-patch-audit-record`.

### First real run (this repo)

`forge ledger rollup` over this project's own transcript directory
(2.4s, 291 files, 1.4 GB), joined to this scratch clone's own history via
`--repo`/`--project`, then `forge ledger audit --record` over the trailing
7-day window:

| metric | n | baseline value |
|---|---|---|
| `median_hook_ms` | 0 | omitted -- no `hook_rollup` rows in this window (debt below) |
| `resend_bytes_per_session` | 20 | 8,815,583 bytes |
| `tokens_per_landed_pr` | 72 | 7,850,228.7 |

A second, unmodified check against that just-recorded baseline reproduces
`resend_bytes_per_session` and `tokens_per_landed_pr` exactly
(`delta_pct: 0.0`, `RATCHET_HELD`) -- both compute in under 5ms once the
rollup exists. `median_hook_ms` has no `hook_rollup` rows at all in this
window: this project's transcript directory (the only input this step is
told to read) carries no hook-telemetry-kind JSONL files, so the metric is
honestly `NO_DATA` rather than a fabricated number. That makes the overall
recorded-window status `NO_DATA` (the worst of the three, per the ranking
above) rather than the `RATCHET_HELD` the design's exit criterion names --
debt, tracked in the PR body, not something this step invents data to hide.
