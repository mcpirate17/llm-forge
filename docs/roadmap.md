# llm-forge roadmap: mostly Rust, built to cut agent cost and raise code quality

Status as of 2026-09-13; source of truth for llm-forge direction

Synthesized 2026-09-13 from three measured reports in this directory:
`hook_token_cost_audit.md`, `agent_efficiency_gaps.md`, `rust_port_plan.md`.

## Baseline (measured on the LLM project's own sessions)

| What | Number |
|---|---|
| Spend share that is cache reads (40 largest sessions) | 66 % (16.77 B tokens); output is 16 % |
| Mean resident context per turn | ~180 K tokens |
| CLAUDE.md + MEMORY.md resend in one 947-call session | 11 times, ~55 K tokens |
| Genuine hook text in that session | ~9.5 K tokens |
| Fixed hook latency per Bash call | ~55 ms pre + ~48 ms post; real hook logic 17 + 5.5 ms; the rest is two CPython starts |
| Hook dispatch path native code | 0 % (75 native functions exist elsewhere; both crates are cdylib-only, no binary) |
| Trivial native binary start | 1 ms vs ~40 ms Python dispatcher |
| Gate: equivalence-probe share of wall | 99.94 %, severity low (cost without authority) |
| Gate check cache hit rate | 9–11 %, 99 MB dead cache |
| context_telemetry | dead since 2026-08-31 (hit its 10 MiB cap, no rotation) |

Conclusion: cost = resident context × turns. Speed = process starts × tool calls plus one unbounded gate check. Neither is measured today.

## Phase 0: quick wins, one PR each (this week)

| # | Item | Owner | Size |
|---|---|---|---|
| 0.1 | Fix `context_telemetry`: rotate at cap, log per-hook `elapsed_ms`, record the SessionStart/compaction `instructions` resend bytes, `summarize` reports ms/hook and bytes/session | DONE PR #15 f36be32 | S |
| 0.2 | `native/forge` binary skeleton: `forge hook <event>` reads the harness JSON, times itself, delegates to the Python dispatcher until handlers are ported; bootstrap points settings at it | DONE PR #14 b285ae3 | S |
| 0.3 | `conductor doctor --harness`: verify/repair the settings that dominate cost (subagent prompt-cache TTL 1h, output bounds, hook wiring) | GLM flash — DONE PR #24 d4e4431 | S |
| 0.4 | CI caching: `actions/cache` for uv, cargo, PMD, npm; sccache for crates | GLM flash — DONE PR #23 d723ea8 (sccache deferred; rust-cache already covers cargo) | S |
| 0.5 | Universal tool-output spill: apply `post_bash_quiet`'s head/tail/spill-to-disk to Read, Grep and MCP results, hard bound, pointer to the spilled file, retrieval CLI | DONE PR #19 bfde3c8 (Python reference + 6-case parity corpus; Rust port in Phase 1 step 6) | M |
| 0.6 | Dogfood campaigns for every module, batched | GLM slice A: batches 1-2 landed (#13, #16), batch 3 running | M |
| 0.7 | Configurable notes root + docs for targets/bootstrap | GLM flash — DONE PR #21 3ee014b | S |
| 0.8 | Trim MEMORY.md index | DONE 2026-09-13: 17,575 -> 16,643 bytes (5%); only 4 stale entries existed, the rest are durable rules, so the real lever is fewer compactions, not a shorter index | S |

## Phase 1: the hook path in Rust (Claude, 9 PRs, each < 1250 lines/file)

1. `forge` binary crate lands (0.2).
2. Port `_bash_guard.py` + `bash_write_targets.py`. DONE PR #20 d2a3b2c: 49 ms -> 1.05 ms, opt-in via FORGE_NATIVE_HOOKS until step 3.
3. Port `_bash_impact.py`. DONE PR #22 78344f0; no wall-clock win yet, three Python hooks still force one interpreter start.
4. Port `crg_gate.py` reusing `branch_policy.rs` claim/path logic. DONE PR #28 5672ff6 (all Bash PreToolUse hooks native, 44 ms -> 1 ms).
5. `forge hook PreToolUse` runs 2–4 natively; shells to Python only for unported hooks. DONE PR #28 5672ff6.
6. Same for PostToolUse (`post_bash_graph`, `post_bash_quiet`, telemetry). DONE PR #31 25cd503 (`post_bash_quiet`/`post_tool_quiet` native via the splice); the matcher-`.*` remainder is step 9 below.
7. Fix and port `workspace_hygiene`'s claim read (today: hardcoded `origin/master`, `ROOT` resolving into `src/`, and a second interpreter re-exec that dominates its 0.7 s). DONE PR #32.
8. Extend `mutation_manifest.rs` to own `mutation_campaign_generate plan`. DONE PR #33 a5f3a62 (forge mutation plan 1.1 ms median, zero interpreter starts).
9. PostToolUse fully native for every non-edit tool (`crg_refresh_report_post`, `read_budget`, `post_bash_graph`, `context_telemetry`; Read/Bash `ls`/Grep/MCP answered with zero interpreter starts). DONE PR #36 (those four; Bash `ls` 1.05 ms, Read 0.86 ms, zero interpreter starts). PR #40 finishes it: the edit family (`crg_graph_refresh`, `post_edit`, `obsidian_post_edit` post-edit path) ported too, so all nine registry PostToolUse names are native and Edit/Write/NotebookEdit answer with zero interpreter starts -- Edit hook overhead 0.83 ms excluding the external formatter (was ~41 ms through the Python dispatcher; formatters 4-5 ms each measured separately), zero python execve on the hook path (the detached refresh worker Python spawns stays, by design, off the hook path).
10. Last: `candidate_review` checks/engine/verification.
Exit criterion: per-tool-call fixed cost < 5 ms, measured by 0.1.

## Phase 2: bet B1, cost ledger (Claude opus design, Rust build)
Rust JSONL reader over harness transcripts: tokens by category (cache read/write, output), resident-context attribution per turn (which attachment, which tool result, which reminder), per-session and per-agent totals, a budget ratchet in the gate (fail a PR that regresses median hook ms or resend bytes/session). Model-routing policy at the harness seam (cheap tier for Explore/clerical) follows from the ledger's numbers. Design doc landed: `docs/design/cost_ledger.md` (data model, attribution method with a declared error bound, gate ratchet, 6-step build plan).

Build plan (design section 6):
1. Reader + schema (`native/forge/src/ledger/{schema,reader}.rs`, `forge ledger read`). DONE PR #35: parses all 5 sessions measured in the design doc with zero panics and matches its cache_read/cache_creation/output sums exactly; unit tests on malformed/truncated/unknown-block lines.
2. Turn/session/hook rollups (`forge ledger rollup`; `agent_rollup` is step 4). DONE: writes `turn_attribution`, `session_rollup`, `hook_rollup` JSONL with compaction and resend detection; verified against all 5 sessions measured in the design doc (n_turns and total_cache_read match exactly for the ~100 MB session, 1.77s/22 MB peak RSS dry-run).
3. Calibration harness (`forge ledger calibrate`, GLM). DONE PR #42: reader unit fix (tool_use/image counted in chars, every split field chars); `forge ledger calibrate sample` (deterministic stratified sampler, per declared session_id, shapes only); `conductor.ledger_calibrate` shim (API-visible window since last compaction marker, count_tokens per block-type group in isolation + whole input, cached; `--offline` chars-per-token per session and overall; exit 2 without a key, never a fabricated bound); fixture embedded at build time — measured `per_block_type` renames `estimate_method` to `byte_proportional_calibrated_<date>` and stamps `estimate_error_pct`, null keeps the uncalibrated label. Real offline run: 45 turns, 5 sessions (the five top-level design-table session files), 0 API calls, overall cpt median 0.772 (p10 0.212, p90 1.059) -- transcripts carry no system prompt/tool defs/attachments, so the ratio calibrates transcript-chars per billed token including that overhead (documented, `docs/ledger.md`). Debt: count_tokens calibration not run: no ANTHROPIC_API_KEY -- per_block_type ships null, bound unmeasured.
4. `agent_rollup` + `Agent:` trailer join to landed PRs. DONE PR #38: `native/forge/src/ledger/landed.rs` shells out to `git log --first-parent main` for `sha`/`merged_at`/`pr_number`/`agent_names`/`harness_session_ids`/shortstat; `agent.rs` joins to `session_rollup` (primary `session_url`, fallback `time_window` restricted to sessions with no `harness_session_ids` of their own, `glm`-named commits additionally restricted to `glm` sessions, `ambiguous` flag, never cross-project); `forge ledger rollup --repo <path> --project <name>` writes `agent_rollup/<date>.jsonl`. Verified against this repo's own last 30 landed commits (2.2s over 291 real transcript files, 1.4 GB); 3 commits hand-cross-checked; a `time_window` over-attribution mode was found, fixed, and re-verified -- 14/30 commits are now honestly `unjoined` (GLM's own work is not logged in this project's transcripts) rather than mis-credited to an unrelated coordinator session (`docs/ledger.md`'s real-data section).
5. Gate phase `cost_budget_audit`. DONE PR #39: `native/forge/src/ledger/audit.rs` (`forge ledger audit`) computes the weighted-median hook latency, mean resend bytes/session and tokens/landed-PR over a trailing window and ratchets each against a recorded baseline (PASS/RATCHET_HELD/REGRESSION/NO_BASELINE, empty window fails loud NO_DATA/exit 3); `src/conductor/cost_budget_audit.py` wraps it as gate phase `cost-budget-audit` (`ok` iff PASS or RATCHET_HELD) and `conductor.mk` targets `cost-budget-audit`/`cost-budget-record`. First real run on this repo's own transcripts: `resend_bytes_per_session` (n=20) and `tokens_per_landed_pr` (n=72) both recorded and RATCHET_HELD on re-check; `median_hook_ms` is honestly `NO_DATA` -- this project's transcript directory carries no hook-telemetry JSONL, so the overall recorded-window status is `NO_DATA` rather than the design's stated `RATCHET_HELD`-for-all-three, tracked as debt in the PR (`docs/ledger.md`'s audit section).
6. CLI plumbing + docs (GLM). DONE PR #43: `python -m conductor.cost_ledger <read|rollup|landed|audit|record|report>` forwards verbatim to the native binary (never re-implements a computation) with Pydantic-v2 glue config (ledger root, repo path, project); `conductor.mk` `ledger-rollup`/`ledger-report`, and `cost-budget-audit` now depends on `ledger-rollup` (idempotent rollup, so the audit always reads fresh rows); native SessionEnd handler rolls up only the ending session's transcript through the existing hook dispatch (2 s kill bound, `FORGE_LEDGER_DISABLE=1` hatch, stderr-only failures) and rides the telemetry dir along so `hook_rollup` populates; `forge ledger prune --ledger-root DIR [--keep-days 90] [--apply]` (dry-run default, monthly `archive/<year-month>.jsonl` for the two aggregates only); `context_telemetry` default moved out of the checkout to `<ledger_root>/telemetry/` in both the Rust writer and the Python twin (`CONTEXT_TELEMETRY_PATH` still honoured, records byte-identical); `docs/ledger.md` rewritten as the handbook (tables, field lists, audit statuses, calibration rendering rule, retention, known limits).

Phase 2 exit table (all six steps landed; baseline values from `ledger/cost_budget_baseline.json`, recorded 2026-09-13T09:32:45Z over the 7-day window 2026-09-07..2026-09-13):

| step | deliverable | PR |
|---|---|---|
| 1 | `forge ledger read` (reader + schema) | #35 |
| 2 | `forge ledger rollup` (turn/session/hook tables) | #37 |
| 3 | `forge ledger calibrate` (sampler + shim + embedded fixture) | #42 |
| 4 | `agent_rollup` + `Agent:`/`Claude-Session:` join to landed PRs | #38 |
| 5 | `forge ledger audit` + gate phase `cost-budget-audit` | #39 |
| 6 | CLI shim, SessionEnd rollup, retention, `docs/ledger.md` | #43 |

| metric | baseline | n |
|---|---|---|
| `resend_bytes_per_session` | 8,815,583 bytes | 20 |
| `tokens_per_landed_pr` | 7,850,228.694444444 | 72 |
| `median_hook_ms` | omitted at record time (no `hook_rollup` rows yet -- step 6's SessionEnd telemetry rides the rollup from now on) | 0 |

## Phase 3: model routing at the dispatch seam (follows from the ledger's numbers)
Cheap tier for Explore/clerical dispatches by default, measured before it is enforced.

1. Ledger plumbing for routing evidence: subagent identity (`agent-<id>` keying, `parent_session_id`), the `task_dispatch` table (one row per `Agent` tool_use: tier requested vs. used, billed tokens, over-cap), the subagent walk (`<dir>/<session>/subagents/agent-*.jsonl`, `--no-subagents` to opt out) and `--branch` for non-main repos. DONE PR #45.
2. Routing policy: which tier a dispatch gets by default, from the measured per-tier dispatch costs. (Claude)
3. The hook that applies it at the dispatch seam. (Claude)
4. A gate metric that ratchets routing cost. (Claude)

## Phase 4: bet B2, correct incremental verification (Claude)
Transitive closure index (Rust) over imports and fixtures, per-test timing DB, flake ledger from the 1,494 receipts, content-addressed check cache keyed per file not per tree. Bound or de-scope the equivalence probe. Exit: gate time proportional to the diff, not the repo.

## Phase 5: bet B3, sandbox runner (Claude, after Tim's design nod)
Landlock/seccomp runner with write scope bound to the session's claims; replaces command-string heuristics as the safety floor.

## Do not build
Another duplication detector (four ship); changed-line coverage (exists); another repo map (code-review-graph covers it); LLM-as-judge as a blocking gate; aggressive context compression (measured to raise cost 6.8 % and halve patch success); Bazel/Nix; a fifth mutation engine.

## Ownership rule
Claude agents (≤150K each): anything Rust, anything that needs a design decision. GLM sessions: campaigns, config plumbing with a known pattern, docs generated from code, CI wiring, CI-failure fixes.

## Mutation platform fixes landed 2026-09-13 (GLM slice E)

- PR #25 ff1d7d8: `refresh` accepts campaigns admitted with extra tests (no more `--force` ratchet resets); `snapshot_worktree` keeps every tracked file under `tests/`, `fixtures/` and `test_*` paths plus `[tool.conductor].snapshot_extra_suffixes`; the generator pairs tests by package-relative path, not basename.
- PR #26 3e536f7: `campaigns/registry.d/<campaign-id>.json`, one file per row; `registry.json` is an empty read-only envelope; `conductor.mutation_registry_split` is the one-shot migration. Concurrent PRs no longer conflict on the registry.
- Slice G: a timed-out mutant is a measurement, not an engine error — TIMED_OUT counts beside `no_coverage`/`unviable` and never blocks the ratchet; the per-mutant bound is 3x the baseline suite's wall time (floor 60 s) unless the manifest pins one, and the resolved value lands in the receipt.
- Slice H: a registered receipt whose `source_sha256` disagrees with the audited tree is a blocking stale-evidence finding (PR #28 passed on one), and a changed file inside measured territory that no campaign pins is reported uncovered; snapshots export the host interpreter as `CONDUCTOR_SNAPSHOT_PYTHON` so Rust campaigns whose tests drive Python pass baseline in a sandbox.
- PR #46: `verify-evidence` expands slim receipts before validating them — the `mutants`/`test_value` lists PR #41 folded under `detail` are restored for the gate's cross-checks (87 of 129 test files' receipts were being rejected as "mutants must be a non-empty list" against real engine PASSes), superseded pointers are rejected naming their replacement, and the reader change is recorded in the runner lineage (`slim-receipt-evidence-expansion-20260913`) so existing receipts stay evidence. No CI job runs `verify-evidence` yet (debt).
- PR #47: CI runs the evidence check — a `mutation-evidence` job verifies the tests a PR changed against current receipts (exit 6 = debt for the PR body, exit 5 = a receipt the validator cannot read) plus a repo-wide decode canary, on rejections classified at their producer (`{receipt, kind, detail}` + `rejection_counts`, evidence-check schema v2).
- Open: re-runs of campaigns that mutate other lanes' files need `--base` pinned to the pre-lane commit.


## Landed this week

| PR | Squash SHA | Title |
|---|---|---|
| #13 | 639e93e | test(mutation): dogfood campaigns batch 1 |
| #14 | b285ae3 | feat(forge): native hook launcher binary, step 1 of the Rust hook path |
| #15 | f36be32 | feat(telemetry): rotate at cap, record hook ms and instructions resends, richer summarize |
| #16 | 4c44c7c | test(mutation): dogfood campaigns batch 2 |
| #17 | 837c40c | test(mutation): dogfood campaigns batch 3 |
| #18 | 095d708 | fix(hooks): port the dispatch launcher and repair the layout-bound test failures |
| #19 | bfde3c8 | feat(hooks): bound Read, Grep and MCP tool output with head/tail and a spill pointer |
| #20 | d2a3b2c | feat(forge): native Bash guard and write-target extraction, Python only for unported hooks |
| #21 | 3ee014b | feat(paths): configurable notes root; docs for targets and bootstrap |
| #22 | 78344f0 | feat(forge): native _bash_impact, Bash PreToolUse native by default |
| #23 | d723ea8 | ci: cache uv, cargo, PMD and npm between runs |
| #24 | d4e4431 | feat(doctor): --harness verifies the settings that dominate agent cost |
| #25 | ff1d7d8 | fix(mutation): refresh accepts extra-test campaigns; snapshots keep fixture trees |
| #26 | 3e536f7 | refactor(mutation): one registry file per campaign, no more registry.json conflicts |
| #27 | 83504e7 | docs: roadmap, registry.d layout, doctor --harness |
| #28 | 5672ff6 | feat(forge): all Bash PreToolUse hooks native (steps 4-5) |
| #29 | 47624e6 | fix(mutation): timed-out mutants are a status, not an engine error |
| #30 | 494153a | fix(mutation): stale receipt hashes block the audit; snapshots reach the host interpreter |
| #31 | 25cd503 | feat(forge): PostToolUse output bounding native, no Python start on PostToolUse |
| #32 | ab153e2 | feat(forge): workspace_hygiene reads the configured integration branch, runs native at SessionStart |
| #33 | a5f3a62 | feat(mutation): campaign plan is native, Python keeps only the CLI |
