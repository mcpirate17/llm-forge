# The cost ledger: `forge ledger`

Every later spend decision (model routing, hook budgets, cache policy) needs
one instrument: tokens by category, attributed per turn, rolled up per
session, per agent and per landed PR, with a ratchet that fails a PR when the
numbers regress. `forge ledger` is that instrument. The commands live in the
native binary (`native/forge/src/ledger/`); `python -m conductor.cost_ledger`
is the forwarding shim and `make ledger-rollup` / `make ledger-report` /
`make cost-budget-audit` the everyday flows. Design:
`docs/design/cost_ledger.md`.

**Model, in one paragraph.** Harness transcripts are the source of truth:
one JSONL file per session, one line per event. The reader streams a file and
reduces it to per-turn summaries (tokens by category, chars by block type);
the rollup turns those into day files under a ledger root —
`<ledger_root>/<table>/<utc-date>.jsonl`, one JSON object per line,
idempotent per session per day (rerunning replaces that session's or hook's
rows for that day, never duplicates them). `agent_rollup` joins sessions to
landed commits through the `Agent:`/`Claude-Session:` trailers; `audit`
ratchets three metrics over a trailing window against a tracked baseline. A
native SessionEnd handler keeps the ledger current without anyone running
make: when a session ends, its transcript is complete and will never change,
so the hook rolls it up then (2 s bound, best-effort to stderr, escape hatch
`FORGE_LEDGER_DISABLE=1`).

## Tables

`<ledger_root>` (env `LEDGER_ROOT`, else `/mnt/data/llm/ledger/`) holds six
tables plus `archive/`. Day-file names are UTC dates; a session's rows land
under the day of its own timestamps.

### `turn_attribution`

One row per usage-bearing turn: `session_id`, `turn_index`, `turn_uuid`,
`timestamp`, `model`, `input_tokens`, `output_tokens`,
`cache_read_input_tokens`, `cache_creation_input_tokens`,
`bytes_by_block_type` (per-turn char counts; every split field is chars:
`tool_use` counts the serialized `input` JSON plus the tool name, `image`
counts base64 payload chars with URL sources at 0; `thinking` is excluded
from the split per design section 3), `estimated_tokens_by_block_type` (the
turn's billed input tokens redistributed across blocks by byte share,
`thinking` excluded), `estimate_method`, `estimate_error_pct` (see
[Calibration](#forge-ledger-calibrate-step-3): `null` while uncalibrated --
an estimate never travels without its error *or* its named absence).

### `session_rollup`

One row per session: `session_id`, `project` (the transcript file's parent
directory name -- for a subagent file, the project directory three levels up,
so `<project>/<uuid>/subagents/agent-x.jsonl` reports `<project>`, not
`subagents`), `first_ts`, `last_ts`, `n_turns`, `n_compactions`,
`total_input`, `total_output`, `total_cache_read`, `total_cache_creation`,
`resend_bytes`, `resend_events`, `harness_session_ids` (every
`session_[a-zA-Z0-9]+` id the transcript itself mentions -- the
`agent_rollup` join key), `commit_subject_digests` (sha256-truncated
digests of the commit subjects this session's `Bash` commands typed --
`git commit -m`, a `-F -` heredoc's first line, `gh pr create --title`;
never the text, and refused outright for subjects shorter than 12
normalized characters -- the second `agent_rollup` join key), `models`
(distinct model values, sorted). A subagent row (`agent-<id>`) keeps its
own digest list; the join credits a subagent match to its parent too.

**Subagent identity rule.** Every line of a subagent transcript
(`<session-uuid>/subagents/agent-<17 hex>.jsonl`) carries its PARENT's uuid
as `sessionId` -- keying a subagent by that value would collapse every
subagent of one parent into a single row. A subagent's rows therefore key
themselves `agent-<agentId>` (unique per file, from the first line that
carries an `agentId`) and name the parent in `parent_session_id`, with
`is_subagent: true`. All three fields are omit-when-unset, so a top-level
session's row is byte-identical to before subagents were modelled. The join
dedupes by `session_id`, so each subagent now counts as its own session --
which is what made `agent_rollup.cap_breaches` able to see a subagent that
alone burned past the cap.

Compaction detection prefers the harness's `isCompactSummary` marker (one per
compaction, unambiguous); the `cache_read_input_tokens` sharp-drop heuristic
only applies to a session with zero markers.

### `hook_rollup`

One row per hook name per telemetry file: `hook_name`, `event`, `n_calls`,
`p50_ms`, `p90_ms`, `total_output_bytes`. Input is the context-telemetry
JSONL the hooks themselves write (see
[Telemetry](#telemetry-context_telemetry)); filed under the day the rollup
command ran.

### `agent_rollup`

One row per agent: `agent_name`, `tier` (substring match over `models`:
`fable`/`mythos` > `opus` > `sonnet` > `haiku` > `glm`, else `unknown`;
disagreement across an agent's sessions is `mixed`), `n_sessions`,
`n_landed_prs`, `total_tokens` (`token_basis: "billed_noncache"` = input +
output + cache_creation; cache_read is billed once, steeply discounted, kept
separately in `total_cache_read`), `token_basis`, `tokens_per_landed_pr`
(`null` when `n_landed_prs` is 0), `cap_breaches` (credited sessions over
`--cap`, default 150000 billed tokens), `join_method` (the strongest evidence
behind any credit: `session_url` > `commit_subject` > `time_window` >
`unjoined`).

Join order, per landed commit: (1) `session_url` -- the commit's
`harness_session_ids` intersect a session's own; exact evidence. (2)
`commit_subject` -- only when (1) finds nothing: a same-project session
whose `commit_subject_digests` contain the commit subject's digest. The
session that typed a commit holds its subject in a `Bash` tool_use (the
`git commit -m` value, a `-F -` heredoc's first non-empty line, or the
`gh pr create --title` value; a squash-merge's ` (#N)` suffix is stripped
before hashing on the landed side), both sides hash with the same
normalization (`subject.rs`), and only the digest ever crosses the reader
boundary. A subagent row that matches credits BOTH itself and its parent
session (per the subagent identity rule), so a dispatch and the subagent
that typed for it are never split across agent rows. More than one session
holding the same digest (a coordinator and its worker both typing the PR
title) credits all of them and flags the commit `ambiguous` -- reported,
not guessed. (3) `time_window` -- only when (2) also finds nothing: a
same-project session whose `[first_ts, last_ts]` overlaps
`[merged_at - 6h, merged_at]`, with two restrictions found the hard way on
real data: a fallback-eligible session must carry no
`harness_session_ids` of its own, and a `glm`-named commit only falls back
to a session whose `models` mention `glm`. More than one overlapping
session credits all of them and flags the commit `ambiguous`. (4)
`unjoined` -- printed to stderr, a finding, never hidden by loosening the
match. Every step only ever looks at sessions whose `project` equals this
run's `--project`.

### `task_dispatch`

One row per `Agent` tool_use in a *top-level* transcript (nested dispatches
-- a subagent dispatching its own subagent -- are out of scope until a
consumer needs their parent linkage): `parent_session_id` (the dispatching
line's session), `dispatch_ts`, `tool_use_id` (the day-file key), `agent_id`
(from the matching `tool_result`'s `agentId: <17 hex>` line, `null` when the
result never arrived or named no id), `subagent_type`, `description` (the
only text stored -- `prompt` and block content never cross the reader
boundary), `model_requested` (`null` when the call set no model), then the
transcript-side fields, all `null` when unjoined: `model_used` (distinct
model values in the subagent's file, sorted), `tier` (the same tier table as
`agent_rollup`), `n_turns`, `billed_tokens` (`billed_noncache`: input +
output + cache_creation), `total_cache_read`, `over_cap` (billed >
`--cap`), `first_ts`, `last_ts`. An unjoined row keeps its dispatch fields
and nulls the rest -- an honest null, never a zero that reads like a
measured empty session. Filed under the day of `dispatch_ts`; a dispatch
with no timestamp is a loud error, not a guessed day.

This is the routing evidence Phase 3 (model routing) decides on: which tier
a dispatch *asked for* (`model_requested`) vs. which actually ran
(`model_used`), at what cost per dispatch, over cap or not.

### `archive/<year-month>.jsonl`

Retention's landing zone for aged aggregate rows; see
[`forge ledger prune`](#forge-ledger-prune-step-6).

## `forge ledger read`

```
forge ledger read <path>... [--json | --summary] [--kind transcript|telemetry]
```

Parses one or more JSONL files into a summary, streaming with `BufRead::lines`
so a 100 MB transcript never loads whole into memory. Never panics on a
malformed or truncated line; such lines are counted and reported
(`skipped_lines`, with line numbers). `--summary` (default): one `key=value`
line per file. `--json`: one `TranscriptSummary`/`TelemetrySummary` JSON
object per file -- the input the rollups consume. `--kind` forces the input
kind instead of detecting it from the first parseable line (a transcript line
always carries `uuid`; telemetry lines carry a top-level `event` and none).

## `forge ledger rollup`

```
forge ledger rollup <path>... [--out <ledger_root>] [--repo <path> --project <name>] \
    [--cap <n>] [--since <date> | --last <n>] [--branch <name>] [--no-subagents] [--dry-run]
```

A path may be a file or a directory walked non-recursively for its immediate
`*.jsonl` children, plus -- since PR #45 -- each child directory's
`subagents/agent-*.jsonl` (the layout the harness moved subagent transcripts
to; 521 of them sat unread under the LLM project on 2026-09-13, and every
one of them keyed itself into its parent's row before the identity rule).
`--no-subagents` restores the flat, pre-PR-45 walk exactly.
`--out` defaults to `$LEDGER_ROOT`, else `/mnt/data/llm/ledger/`.
`--dry-run` prints the rows as JSONL and writes nothing.

`--repo` scans that repository's `git log --first-parent <branch>` (see
[`forge ledger landed`](#forge-ledger-landed-step-4)) and joins it to this
run's `session_rollup` rows into `agent_rollup`; without it `agent_rollup` is
not computed. `--project` (required with `--repo`) is the transcript
directory basename this repo's commits may join against -- no default,
because guessing wrong either drops every join silently or crosses a project
boundary. One invocation joins exactly one project. A project name that
begins with `-` (every munged path does: `-home-tim-...`) needs the `=` form,
`--project=-home-tim-Projects-LLM`, or the value parses as a flag. `--cap`,
`--since`/`--last` and `--branch` pass through to the landed scan.

## `forge ledger landed` (step 4)

```
forge ledger landed --repo <path> [--since <date> | --last <n>] [--branch <name>]
```

One JSON row per landed first-parent commit: `sha`, `merged_at` (committer
date, UTC, `YYYY-MM-DDTHH:MM:SSZ` -- `%cd` with `--date=format-local:...` and
`TZ=UTC`, never `%cI`, which ignores `--date`), `pr_number` (trailing `(#N)`
in the subject, `null` when absent), `agent_names` (every `Agent: <name>`
trailer, sorted, deduplicated -- a squash commit can carry several),
`harness_session_ids`, `files_changed`, `insertions`, `deletions`.

`--branch` names the integration branch. Default: the ref
`refs/remotes/origin/HEAD` points at, falling back to `main` when a repo has
no origin/HEAD at all -- the LLM monorepo integrates on `master` and has no
`main`, which is why the default is resolved rather than hardcoded.

## `forge ledger calibrate` (step 3)

The rollup's split assumes uniform chars-per-token across block types; this
step measures how wrong that is and ships the bound beside every estimate.
Two halves, split along the network boundary:

```
forge ledger calibrate sample <transcript>... --per-session N --seed S
uv run python -m conductor.ledger_calibrate <sample.jsonl> <transcript>... \
    [--model M] [--sleep-ms 200] [--offline] [--out F] [--cache C]
```

The sampler picks turns deterministically, stratified per *declared*
`session_id`, shapes only. The Python shim re-reads each sampled turn's
API-visible window (messages since the last compaction marker, up to the
turn's own line) and either calls `count_tokens` per block-type group
(**online**; responses cached in a job-local JSON, so a rerun costs zero
calls; without `ANTHROPIC_API_KEY` it prints exactly what is missing and
exits 2 -- it never fabricates a bound) or computes
`chars_per_token = window chars / billed input` per turn (**`--offline`**:
no network, no SDK, no key).

The result lands in `native/forge/tests/fixtures/ledger/calibration.json`
(`{generated_utc, model, n_turns, per_block_type | null, whole_input_mape |
null, chars_per_token {median, p10, p90}}`), which `rollup.rs` embeds at
build time: a measured `per_block_type` renames every `turn_attribution`
row's `estimate_method` to `byte_proportional_calibrated_<date>` and stamps
the per-block error into `estimate_error_pct`; a `null` one keeps the
uncalibrated label and a `null` error. A corrupt fixture fails the build
rather than silently downgrading.

**Rendering rule.** Every estimate is quoted as `± N%` (the MAPE) *only*
when the fixture's `per_block_type` is non-null; while it is null the bound
is stated as **"bound not measured"**, never as a bare figure.

### First real run (the committed fixture, offline)

Inputs are the five top-level design-table session files (a resume file and
a subagent transcript are different populations). Exact commands, so the
fixture reproduces byte-for-byte:

```
forge ledger calibrate sample \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/206702fb-97d8-444b-97c3-5d12c6eeb6a8.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/3c0c3659-9c0a-43c6-8826-cbba537595f1.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/5e93df87-d437-4c17-adaa-75357a1dc0c5.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/65f84759-e8ac-47fa-a82c-d67624da005d.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/c38ffd05-637b-4717-bf6d-2bf44203793a.jsonl \
  --per-session 10 --seed 20260913 > /tmp/calibration_sample.jsonl

uv run python -m conductor.ledger_calibrate /tmp/calibration_sample.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/206702fb-97d8-444b-97c3-5d12c6eeb6a8.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/3c0c3659-9c0a-43c6-8826-cbba537595f1.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/5e93df87-d437-4c17-adaa-75357a1dc0c5.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/65f84759-e8ac-47fa-a82c-d67624da005d.jsonl \
  /home/tim/.claude/projects/-home-tim-Projects-LLM/c38ffd05-637b-4717-bf6d-2bf44203793a.jsonl \
  --offline \
  --out native/forge/tests/fixtures/ledger/calibration.json \
  --cache /tmp/.ledger-calibrate-cache.json
```

45 turns, 5 sessions (10/session; `c38ffd05` holds only 5 usage-bearing
turns), 0 API calls. The fixture holds, exactly:

| field | value |
|---|---|
| `chars_per_token.median` | 0.771596729265882 |
| `chars_per_token.p10` | 0.21247460867487156 |
| `chars_per_token.p90` | 1.058955483646396 |
| `per_block_type` | `null` (**bound not measured**: no `ANTHROPIC_API_KEY` on the recording machine) |

Read the numbers knowing what the window holds: transcripts do not carry the
system prompt, tool definitions or attachment records, yet all of it is
billed as input, so the ratio calibrates *transcript-chars per billed input
token* including that overhead -- which is exactly the constant the resend
heuristic wants, and why the overall median is 0.77 rather than the naive
~4. Per-session medians at sample time: `206702fb` 0.871, `3c0c3659` 0.776,
`5e93df87` 0.954, `65f84759` 0.663, `c38ffd05` 0.008 (8 message lines
against 69K billed cache-read tokens of non-transcript overhead). Honest
calibration findings, not sampler bugs; the per-block-type bound, once
measured with a key, rests on the same window definition.

## `forge ledger audit` (step 5)

```
forge ledger audit --ledger-root <dir> --baseline <file> [--window-days N] [--record]
```

Three metrics over a trailing `N`-day window (default 7), recorded or
checked: **`median_hook_ms`** (weighted median of `hook_rollup.p50_ms`,
weighted by `n_calls`), **`resend_bytes_per_session`** (mean of
`session_rollup.resend_bytes`), **`tokens_per_landed_pr`**
(`sum(total_tokens) / sum(n_landed_prs)` over `agent_rollup` rows whose
`join_method` is not `unjoined`). Never rounds a tie up to `PASS`; never
invents a value for a metric with no rows.

| Status | Meaning |
|---|---|
| `PASS` | `value` strictly improved on the baseline. |
| `RATCHET_HELD` | within tolerance (5% at record time) but not improved; a tie is `RATCHET_HELD`, never `PASS`. |
| `REGRESSION` | `value` exceeds `baseline * (1 + tolerance_pct / 100)`. |
| `NO_BASELINE` | a value this window but no recorded baseline entry. |
| `NO_DATA` | no rows in this window (`value` is `null`), whatever the baseline says. |

Overall status is the worst of the three, ranked
`REGRESSION > NO_DATA > NO_BASELINE > RATCHET_HELD > PASS`. If *every* metric
has zero rows the command fails loud: exit `3`, nothing written, `--record`
included. `--record` is the only way the baseline file
(`ledger/cost_budget_baseline.json`, tracked) changes; a metric with no data
at record time is omitted (one stderr line naming it as debt) rather than
recorded as `0`. The file also carries `recorded_utc` and `ledger_root_sha`
(sha256 over every day file read), so a receipt names exactly which rows it
was computed from.

### Gate wiring: `cost-budget-audit`

`src/conductor/cost_budget_audit.py` wraps the command as gate phase
`cost-budget-audit`. The ok rule, verbatim from the module:

```python
OK_STATUSES = frozenset({"PASS", "RATCHET_HELD"})
ok = not any(metric.status == "REGRESSION" for metric in result.metrics.values())
```

`PASS`, `RATCHET_HELD`, `NO_BASELINE` and `NO_DATA` -- including the
hard-empty exit-3 case -- are all `ok`: a fresh clone or CI runner has no
baseline and no ledger rows yet, and there is nothing to regress against, so
`make gate` must not go permanently red. `detail` still names every metric's
status verbatim, never rounded up to `PASS`. Only a missing `forge` binary or
unparseable output raises loud (`CostBudgetAuditError` -> `GateRefusal`). The
direct CLI path keeps failing loud: exit 3 on the hard-empty window, exit 1
on anything but `PASS`/`RATCHET_HELD`.

## `forge ledger prune` (step 6)

```
forge ledger prune --ledger-root DIR [--keep-days 90] [--apply]
```

Retention, exactly the design's "Storage" paragraph: daily files older than
the window are retired. `session_rollup`/`agent_rollup` -- the aggregates --
are archived first, one JSONL per calendar month under
`<ledger_root>/archive/<year-month>.jsonl`, idempotently (a row already
archived, keyed by `session_id`/`agent_name`, is skipped -- the same rule the
day-file writer applies to superseded rows). Raw `turn_attribution`/
`hook_rollup` day files are not worth keeping past the window they were
computed to support and are deleted without archiving. **Dry-run is the
default**: it lists every archive and delete it would perform and touches
nothing; `--apply` is the only way a file leaves the tree. Non-day filenames
inside a table directory are a loud error, not a skip -- that directory is
owned by the writer, and anything else means the layout assumption is wrong.

## Continuous population: the SessionEnd handler (step 6)

`forge hook SessionEnd` (wired through the existing hook dispatch, so no
settings change) runs a native handler before the event delegates to Python
exactly as before: it re-invokes itself (`current_exe`) as
`forge ledger rollup <transcript_path> [<telemetry dir>]`, bounded by a 2 s
wall clock -- the wait happens on a channel and the child is killed by pid on
overrun, a real kill, not a hope -- and every failure is one stderr line:
SessionEnd owns no verdict, so a ledger problem must never change the hook's
exit code. The telemetry directory (default
`<ledger_root>/telemetry/context_telemetry/`, below) rides along when it
exists, which is what populates `hook_rollup` and makes `median_hook_ms`
computable. `FORGE_LEDGER_DISABLE=1` skips the rollup entirely; the hook
itself still runs.

## Telemetry: `context_telemetry`

The hooks' own telemetry (one JSON line per `forge hook` call: `event`,
`elapsed_ms`, `delegated`, `ts`; plus the PostToolUse context-size records)
writes to `<ledger_root>/telemetry/context_telemetry/events.jsonl` --
`CONTEXT_TELEMETRY_PATH` overrides, `LEDGER_ROOT` else
`/mnt/data/llm/ledger/`, in both the Rust writer and the Python twin, with a
byte-identical record format. The default used to live inside the checkout
(`<root>/src/research/tmp/...`, inherited from PR #36): a read-only checkout
must never be a hook's write target, and keeping telemetry under the ledger
root is what lets the SessionEnd rollup feed `hook_rollup` from the same root
it writes tables to. The file rotates at 10 MiB, five rotations kept.

## The Python shim and the make targets (step 6)

```
python -m conductor.cost_ledger <read|rollup|landed|audit|record|report> [args...]
```

Locates the `forge` binary the way `conductor.cost_budget_audit` does
(`resolve_forge_binary`), forwards arguments verbatim, propagates the exit
code, never re-implements a computation. Two conveniences: `rollup` with no
paths fills this project's defaults (the repo's own transcript directory --
`~/.claude/projects/` + the repo path with `/` -> `-` -- rolled up into the
ledger root with `--repo`/`--project`), and `report` renders the audit
verdict as one line per metric with its status, exiting by the same
`OK_STATUSES` rule as `make cost-budget-audit`.

- `make ledger-rollup` -- the default rollup above (`LEDGER_ROOT` overrides,
  default `/mnt/data/llm/ledger/`). Idempotence makes reruns free.
- `make ledger-report` -- the human table.
- `make cost-budget-audit` -- depends on `ledger-rollup`, so the audit reads
  fresh rows instead of a stale or empty root; same for
  `cost-budget-record`.

## Known limits

- **Resend detection is a heuristic, not a measurement**: a turn whose own
  byte total does not exceed the previous turn's yet still pays
  `cache_creation_input_tokens > 0` is counted as re-caching bytes already
  seen; the reader keeps no bytes to prove identity.
- **The token split is byte-proportional and uncalibrated**: billed input is
  redistributed across blocks by byte share at a constant 4 chars/token
  (`cpt4`) while the fixture's `per_block_type` is `null`; the committed
  offline calibration (chars-per-token median 0.771596729265882) corrects
  the constant the resend heuristic wants but not the per-block-type split
  itself -- bound not measured until a `count_tokens` run.
- **The `time_window` join over-credits long-lived sessions by
  construction**: a session resumed across days overlaps every commit merged
  during its life, so the fallback is restricted to sessions carrying no
  `harness_session_ids` of their own (and `glm` commits to `glm` sessions);
  commits that then match nothing are reported `unjoined` rather than
  credited to a bystander. The `commit_subject` tier now sits above it and
  takes most of that load (the session that typed the subject wins over
  every mere time-overlap), but it cannot fix everything: a commit typed in
  a session whose transcript is gone (deleted, or never walked by the
  rollup) has no digest to match, and a subject shorter than 12 normalized
  characters is refused a digest on BOTH sides -- `fix` or `wip` matches
  far too much for the join to mean anything -- so those commits still fall
  through to `time_window` or `unjoined`.
- **Split-file sessions under-count**: one session id whose transcript log
  spans two physical files becomes two `session_rollup` rows; the
  `agent_rollup` join dedupes by `session_id`, so only one row's tokens
  count (under-count, never double-count). Merging them belongs to
  `rollup.rs`, not the join.
- **Compaction/resend never double-signal**: the sharp-drop heuristic only
  runs when a session has zero `isCompactSummary` markers.
- **This repo's own sessions are not under its own project dir**: sessions
  that worked on llm-forge are recorded under the harness's directory for
  `/home/tim/Projects/LLM` (the monorepo this checkout lives inside), not
  under a `-llm-forge` project dir -- `~/.claude/projects/-home-tim-Projects-llm-forge/`
  holds only memory files. Nothing to fix; it just means a rollup of this
  repo's "own" transcript directory finds nothing, by construction.

## The outcome join (Phase 3 step 3, item 3)

`native/forge/src/ledger/outcome.rs::apply()` runs inside `forge ledger
rollup --repo <repo> --project <name>`, right after the `agent_rollup`
commit-join, and fills three fields on every `task_dispatch` row whose
`parent_session_id` is set: `landed: Option<bool>`, `required_rework:
Option<bool>`, `ci_red_on_first_push: Option<bool>`. **Absent the cache
file below, all three stay `null` -- never `false`.** `gh` is not available
to Rust code in this repo, so the join reads a cache file instead of
shelling out live.

### `ci_history` cache schema

One file per repo, `<repo>/ledger/ci_history/<owner>_<name>.json` (the
`owner_name` slug parsed from `origin`'s remote URL, lower-cased,
`.git`-stripped -- `native/forge/src/ledger/outcome.rs::owner_repo_slug`).
When `repo` has no git metadata or no `origin` remote at all (a scratch
checkout mid-setup), that is the same "no data yet" case as the file being
absent, not a hard error.

```json
{
  "fetched_utc": "2026-09-13T00:00:00Z",
  "prs": {
    "50": {
      "branch": "forge/routing-policy",
      "first_push_sha": "aaa111",
      "first_push_ci": "red",
      "commits": [
        {"sha": "aaa111", "subject": "feat: first cut",
         "trailers": {"Agent": "glm"}},
        {"sha": "bbb222", "subject": "fix: CI",
         "trailers": {"Agent": "llm-b0",
                       "Claude-Session": "https://claude.ai/code/session_01X"}}
      ]
    }
  }
}
```

- `prs`: keyed by PR number as a string (matches `gh pr view --json number`
  and this repo's own PR references in commit subjects).
- `commits`: the **pre-squash** commit list on the PR branch, in the order
  `gh api repos/{owner}/{repo}/pulls/{n}/commits` returns them -- `git log
  --first-parent main` only ever sees the single post-squash commit on
  `main`, which is why this join needs its own cache at all rather than
  reading local git history.
- `first_push_ci`: `"green"` / `"red"` / `"unknown"` -- the check-run
  conclusion for the PR's first pushed commit, before any fix-up commits.
- `trailers.Agent` / `trailers.Claude-Session`: parsed the same way the
  gate already parses trailers on a landed commit (`AGENTS.md`'s
  `Agent:`/`Claude-Session:` convention); both optional per commit.

Field derivation in `apply()`:
- `landed`: `true` iff the row's `parent_session_id` matches a
  `CommitJoin` `agent_rollup` already computed for this rollup (i.e. the
  session's work is credited to a landed commit); `false` when the cache
  exists but no join was found (an honest negative, not a gap); never set
  at all when the cache file itself is missing.
- `ci_red_on_first_push`: the landed PR's `first_push_ci`, mapped
  `green -> false`, `red -> true`, `unknown -> null`. `null` when the PR
  landed but is not yet in the cache (fetcher lag).
- `required_rework`: `false` when the PR is a single commit; otherwise
  `true` iff any commit after the first carries an `Agent:` trailer whose
  presumed tier (`docs/roadmap.md`'s Ownership rule: any `glm`-named agent
  is the cheap tier, everything else is not -- a documented approximation,
  not a per-commit measurement) ranks strictly above the row's own `tier`.
  The first commit is treated as the dispatch's own work; the cache has no
  other way to say which commit was whose.

### `ci_history` fetcher, not yet built (GLM slice)

This file is never written by `native/forge` -- a separate GLM-owned CLI
slice fetches it. Requirements for that fetcher:

1. Python CLI/glue only (per `CLAUDE.md`'s language hierarchy); no Rust,
   no compute-heavy logic here.
2. One invocation per repo: `python -m conductor.ci_history_fetch --repo
   <path> --out ledger/ci_history/<owner>_<name>.json`.
3. Uses `gh pr list --state merged --json number,headRefName` then, per
   PR, `gh api repos/{o}/{r}/pulls/{n}/commits` for the commit list and
   `gh api repos/{o}/{r}/commits/{sha}/check-runs` (first commit only) for
   `first_push_ci`.
4. Parses `Agent:`/`Claude-Session:` trailers out of each commit's message
   into `trailers` exactly as shown above; missing trailers serialize as
   absent keys, never empty strings.
5. Merges into the existing file rather than overwriting it wholesale --
   PRs already cached and unchanged (same `first_push_sha`) are left alone,
   so re-runs are cheap and idempotent.
6. Writes `fetched_utc` as the run's own UTC timestamp (RFC 3339).
7. Rate-limit aware: backs off on `gh`'s own 403/secondary-rate-limit exit
   codes rather than treating them as a real per-PR failure.
8. Exits non-zero (loud) on any PR it could not resolve, but keeps every
   already-written PR in the output file -- a partial fetch must still be
   the best data available, not thrown away.
9. Scheduled periodically (a cron/CI job), not run inline by `forge ledger
   rollup` -- the join always reads whatever is on disk *now*.
10. Ships its own test fixture and a dry-run mode (`--dry-run` prints what
    it would fetch/write without calling `gh`) so it can be verified without
    live API credits.

### Row identity: `task_dispatch` is keyed by `agent_id` (fixed, was debt from PR #52)

`native/forge/src/ledger/agent_upsert.rs` (`forge ledger rollup-agent`, the
`SubagentStop`-triggered upsert, `docs/routing.md`'s enforcement section)
has no access to the parent transcript, so it never learns the real
`tool_use_id` a later full `forge ledger rollup --repo` sweep would use to
key that same dispatch's `task_dispatch` row. It writes a synthetic
`tool_use_id` (`agent-<agent_id>`) into the row as a plain field, but both
this live path and the full sweep (`rollup.rs::build_task_dispatch`, which
learns `agent_id` from the dispatch's `tool_result` `agentId:` line) key
their `task_dispatch` day-file upsert by `agent_id` -- the one field they
always agree on. A live row followed by a full sweep of the same dispatch
therefore collapses to exactly one row (`write_day_file`'s existing
supersede-by-key behavior does the merge; no reconciliation pass is
needed), instead of the two rows the mismatched keys used to produce.
