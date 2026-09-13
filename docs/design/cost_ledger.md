# Design: the cost ledger

Phase 2 (bet B1) of `docs/roadmap.md`. Every later cost decision — routing an
agent to a cheap tier, ratcheting a hook's ms budget, trimming resent context
— needs one instrument that answers "did that change actually save money."
This is that instrument's design: what it measures, how it stores it, how
its numbers get attributed when the API reports only totals, how a ratchet
plugs into the gate, and where model routing hooks in. No code ships in this
PR.

## 1. Questions the ledger must answer, ranked

1. **Cost per landed PR, by agent tier.** Sum of `input_tokens +
   cache_creation_input_tokens + cache_read_input_tokens*read_price_ratio +
   output_tokens` across every turn in every session that touched the PR's
   branch, divided per landed PR, grouped by tier (Sonnet/Opus/GLM
   flash/Haiku). This is the number Tim's "minimize cost" goal is graded on.
2. **Resident-context composition per turn.** For a sampled turn: bytes from
   system prompt, `CLAUDE.md`, `MEMORY.md`, tool results, system-reminder
   blocks, hook `additionalContext`, attachments. Answers "what is actually
   in the 180K tokens" (roadmap baseline) instead of assuming.
3. **Resend waste.** Bytes of an attachment or reminder that are byte-
   identical to a prior turn in the same session, re-sent anyway. The
   roadmap's "CLAUDE.md + MEMORY.md resent 11 times, ~55K tokens" is one
   instance of this; the ledger must produce that number automatically, not
   by hand-audit.
4. **Hook ms and tool-output bytes per call.** Per-event `elapsed_ms`
   (`telemetry.rs::record_delegation`/`record_native` already emit this) and
   the bytes a tool result carried, to hold Phase 1's "<5ms fixed cost" exit
   criterion to a number over time, not a one-time measurement.
5. **Compaction count per session and tokens lost to it.** A compaction event
   drops resident context and forces a resend; counting it explains part of
   (3) instead of leaving it as unattributed cache-write cost.
6. **Per-agent totals against the 150K cap.** Every llm-forge brief caps an
   agent at 150K tokens; the ledger is the only way to check, after the
   fact, whether a lane actually stayed under its stated bound.

## 2. Data model

### Reader schema (`native/forge/src/ledger/schema.rs`)

One struct per transcript record type actually observed (Sections 3 below
explains why totals-only records still need per-block accounting):

```rust
/// One line of a Claude Code transcript JSONL file.
struct TranscriptLine {
    uuid: String,
    parent_uuid: Option<String>,
    session_id: String,
    timestamp: String,          // RFC3339, as written by the harness
    message: Option<Message>,
}

struct Message {
    role: String,                // "user" | "assistant"
    model: Option<String>,       // assistant messages only
    usage: Option<Usage>,        // assistant messages only
    content: Vec<ContentBlock>,
}

struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

/// Tagged by `type`; only the byte length of text is read, never the text
/// itself, past the reader boundary -- the ledger stores shapes, not content.
enum ContentBlock {
    Text { char_len: usize, is_system_reminder: bool },
    ToolUse { name: String },
    ToolResult { char_len: usize, tool_use_id: String },
    Thinking,
    Image,
}
```

Hook telemetry reuses the JSONL shape `telemetry.rs::DelegationEvent` and
`context_telemetry.py::hook_timing_event`/`hook_context_event` already emit
(`event`, `elapsed_ms`, `delegated`, category, `content_hash`) — the ledger
reader ingests both transcript JSONL and telemetry JSONL as two input kinds
feeding one output model, not two ledgers.

### Derived tables

- **`turn_attribution`**: one row per assistant `usage`-bearing message —
  `session_id, turn_index, model, tier, input_tokens, output_tokens,
  cache_read_input_tokens, cache_creation_input_tokens,
  bytes_by_block_type{text, tool_result, tool_use, thinking, image},
  estimated_tokens_by_block_type` (Section 3 for the estimate).
- **`session_rollup`**: `session_id, project, first_ts, last_ts, n_turns,
  n_compactions, total_input, total_output, total_cache_read,
  total_cache_creation, resend_bytes, resend_events`.
- **`agent_rollup`**: `agent_name (Agent: trailer), tier, n_sessions,
  n_landed_prs, total_tokens, tokens_per_landed_pr, cap_breaches` — joins
  `session_rollup` to landed PRs via the `Agent:` trailer (KB-GOV-02) on
  merged commits, the only durable record per CLAUDE.md.
- **`hook_rollup`**: `hook_name, event, n_calls, p50_ms, p90_ms,
  total_output_bytes` from the telemetry stream, feeding the Phase 1 exit
  criterion.

### Storage

Append-only JSONL under `/mnt/data/llm/ledger/<table>/<utc-date>.jsonl`,
one file per table per day — JSONL, not Parquet, for the first cut: the
write path is one `serde_json` line per record (matching
`telemetry.rs::append_line`'s existing pattern exactly, no new dependency),
and Polars reads JSONL natively when a report needs columnar speed. Parquet
compaction is a later step (Section 6, "not the first PR") once the JSONL
volume itself becomes the cost. Retention: 90 days of daily files, then a
monthly rollup (`session_rollup`/`agent_rollup` aggregates only — raw
`turn_attribution` is not worth keeping past the window it was computed to
support) written to `/mnt/data/llm/ledger/archive/<year-month>.jsonl` and the
daily files pruned, mirroring `context_telemetry.py::_prune_rotated`'s
keep-N-newest pattern (here keep-90-days).

## 3. Attribution method

The Claude API's `usage` block reports **totals per turn**
(`input_tokens`, `cache_read_input_tokens`, ...), never a per-content-block
split. Splitting "which attachment cost how much" is therefore an estimate,
not a measurement, and must say so everywhere it is surfaced.

**Method: byte-proportional estimate, calibrated against `count_tokens`.**

1. For a turn, sum `char_len` per `ContentBlock` variant (text,
   tool_result, tool_use structural JSON, thinking is never billed as
   input on the *next* turn per Anthropic's docs — excluded from the
   input-side split).
2. Estimated tokens for block type `k` = `usage.input_tokens *
   (chars_k / total_chars)`. This assumes uniform bytes-per-token across
   block types in one turn, which is false at the margin (JSON tool-result
   payloads tokenize denser than prose) — hence step 3.
3. **Calibration**: on a fixed sample (50 turns, stratified by session size:
   10 from each of the 5 sessions in Section "Measured this session"),
   call the Anthropic `count_tokens` endpoint per block in isolation and
   compare to the proportional estimate. Record the mean absolute
   percentage error per block type as the declared error bound; ship the
   bound in every report the ledger renders (`± N%`), never a bare number.
   This calibration is a one-time measurement, re-run whenever the
   tokenizer or pricing model changes (recorded, same discipline as a
   mutation baseline — Section 4).
4. Where a bound is not yet measured (first PR of the build), the ledger
   must refuse to report a per-block split rather than print an
   uncalibrated number silently — the same "no silent fallback" rule
   `CLAUDE.md` states for code applies to numbers.

## 4. Budget ratchet in the gate

Three numbers ratchet first, cheapest to measure and closest to what Phase 1
already changed:

1. **Median hook `elapsed_ms` per event**, from `hook_rollup` — holds Phase
   1's "<5ms fixed cost" claim to a number that can regress.
2. **Resend bytes per session** (Section 1.3) — the single biggest
   identified waste (55K tokens/session baseline) and the most directly
   actionable (SessionStart/compaction wiring).
3. **Tokens per landed PR**, from `agent_rollup` — the number Tim's "minimize
   cost" goal is actually graded on; the other two are levers on this one.

**Baseline discipline mirrors mutation evidence exactly** (KB-MUT-02): the
ledger records a baseline receipt the first time a metric is measured for a
given scope (`RATCHET_HELD`, not PASS — a first run has nothing to compare
against). A subsequent PR's gate run recomputes the same three numbers for
the candidate tree and compares to the last **registered** receipt, not the
live working tree, matching `gate.py::run_gate`'s existing rule that policy
and evidence come from the exported candidate tree so an uncommitted edit
can't change the verdict. A new phase, `cost_budget_audit`, is added to
`run_gate`'s phase list alongside `mutation_corpus_audit` — same shape
(`PhaseResult`, refuse loudly if the ledger data is missing rather than
skip), same registry convention as `campaigns/registry.d/` (one file per
scope under `ledger/registry.d/<scope>.json` pointing at its baseline
receipt) so concurrent PRs never conflict on one shared file (the exact
defect `registry.json` had before PR #26 split it).

**A PR body must show**, in a new table beside the existing Mutation
evidence table:

| Metric | Baseline (registered) | This PR | Delta | Status |
|---|---|---|---|---|
| median hook ms | ... | ... | ... | PASS / REGRESSED / RATCHET_HELD |
| resend bytes/session | ... | ... | ... | ... |
| tokens/landed PR | ... | ... | ... | ... |

`RATCHET_HELD` is reported as exactly that, never rounded up to PASS,
matching the mutation-evidence rule verbatim.

## 5. Model-routing policy seam

The ledger gives the harness, at the point it dispatches an Explore/clerical
task, three signals it does not have today: (a) the requesting session's
`tier` history and its `tokens_per_landed_pr` trend, (b) whether the task
class (Explore, doc generation, CI-fix, config plumbing — the same
categories `docs/roadmap.md`'s Ownership rule already names by hand) has a
recorded cheap-tier success rate above a floor, (c) the current session's
remaining budget against its 150K cap (`agent_rollup`, Section 1.6).

**Routing signal**: task class + remaining budget headroom. A task tagged
Explore/clerical/docs routes to the cheap tier (GLM flash, Haiku) by default;
anything touching Rust, a design decision, or a gate gets Sonnet/Opus,
matching the Ownership rule already in force — the ledger's job is to make
that rule's assumption checkable, not to invent a new one.

**What the ledger must record to prove routing saved money without hurting
quality**: per routed task, `(tier, task_class, tokens_spent, landed: bool,
required_rework: bool, ci_red_on_first_push: bool)`. Two derived numbers
settle the argument: **rework rate** (fraction of cheap-tier PRs that needed
a Sonnet/Opus follow-up commit before landing) and **CI red rate per tier**
(fraction of first pushes that failed CI, from `gh pr checks` history). A
routing policy is not "saved money" if a cheap tier's rework rate erases the
per-token saving — the ledger must show both sides of that trade, not just
the token count, every time the policy is evaluated.

## 6. Build plan for the sonnet agents

Each step ≤150K tokens, one PR, one measurable exit. GLM-ownable steps
marked.

1. **Reader + schema** (`native/forge/src/ledger/{schema,reader}.rs`,
   `forge ledger read <path>...` subcommand parsing transcript JSONL into
   `TranscriptLine`). Exit: parses the 5 sessions measured in this doc
   without error, unit tests on malformed/truncated lines (a session file
   can be mid-write). Claude — new struct design.
2. **Turn/session/agent rollups** (`forge ledger rollup`, writes
   `turn_attribution`/`session_rollup` JSONL). Exit: rollup totals for a
   fixture transcript match a hand-computed expected file byte-for-byte.
   Claude.
3. **Calibration harness** (`forge ledger calibrate`, Python CLI shim calling
   `count_tokens`, since it is a network call and belongs in
   orchestration not the hot Rust reader). Exit: error bound recorded and
   committed as a fixture, per block type. **GLM** (known pattern: call an
   API, write a fixture, no design decision).
4. **`agent_rollup` + `Agent:` trailer join to landed PRs** (git log scan for
   trailers, join to `session_rollup` by time window). Exit: `agent_rollup`
   for this repo's last 30 landed PRs, cross-checked by hand against `git
   log` for 3 of them. Claude (join logic, tier inference is a design
   choice).
5. **Gate phase `cost_budget_audit`** (`src/conductor/gate.py`, `PhaseResult`,
   `ledger/registry.d/` convention, `conductor.mk` target
   `cost-budget-audit`). Exit: first run on this repo produces
   `RATCHET_HELD` baseline receipts for all three metrics; a synthetic
   regression fixture fails the phase. Claude (gate wiring is a design
   decision per the Ownership rule).
6. **CLI plumbing + docs** (`python -m conductor.cost_ledger` thin shim over
   `forge ledger`, `docs/ledger.md` mirroring `docs/mutation.md`'s
   structure, `make ledger-report`). **GLM** (docs generated from code, CLI
   plumbing with a known pattern — mirrors `mutation.md`/`doctor.md`
   already in the repo).

Parquet migration, the model-routing policy's actual dispatcher change, and
per-content-block live attribution (vs. the batch estimate) are explicitly
**not** in these six steps — they are follow-on bets once the ledger's first
three ratchet numbers have a few weeks of real receipts behind them.

## 7. Do-not-build list (carried from `docs/roadmap.md`)

- **Aggressive context compression.** Measured (roadmap) to raise cost 6.8%
  and halve patch success — the ledger exists partly to keep re-litigating
  this settled, not to reopen it.
- **LLM-as-judge as a blocking gate.** A judge call is itself a ledger line
  item (tokens, tier, latency) with no measured floor on its own false-
  positive rate; gating on it would spend budget to protect budget with an
  unverified instrument.
- **Another repo map.** `code-review-graph` already covers this; a second
  index is resident-context cost with no attribution the ledger would show
  as anything but waste.

## Measured this session

Sessions read (sizes, not content — see Section 3 on why block text is
never retained past char-length):

| Session | Bytes | Turns w/ usage | cache_read | cache_creation | output |
|---|---|---|---|---|---|
| `c38ffd05...` | 51,635 | 5 | 69,120 | 0 | 1,225 |
| `agent-ac760022...` (subagent) | 500,062 | 58 | 2,091,243 | 191,136 | 857 |
| `206702fb...` | 26,122,216 | 4,365 | 489,508,057 | 9,214,501 | 3,207,325 |
| `ada28b6b...` | 39,542,514 | 6,499 | 733,478,224 | 13,196,014 | 4,884,628 |
| `5e93df87...` | 100,657,017 | 16,194 | 1,830,085,490 | 32,650,034 | 12,093,490 |

Combined across the 5: cache-read tokens are **97.6%** of the
input+output+cache total (vs. 66% on the roadmap's 40-largest-session
sample — these 5 skew toward long-running sessions, which is exactly why
Question 1's per-tier, per-session breakdown matters more than one repo-wide
percentage). Tool-result content is **95.1%** of text+tool_result
characters across the same 5 sessions — the resident-context composition
question (Section 1.2) is not close in these samples: tool output, not
prose, dominates what a turn resends.
