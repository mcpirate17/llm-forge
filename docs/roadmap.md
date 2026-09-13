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
4. Port `crg_gate.py` reusing `branch_policy.rs` claim/path logic. IN PROGRESS forge/rust-bash-remaining (with step 5 and the two other Bash hooks).
5. `forge hook PreToolUse` runs 2–4 natively; shells to Python only for unported hooks.
6. Same for PostToolUse (`post_bash_graph`, `post_bash_quiet`, telemetry).
7. Fix and port `workspace_hygiene`'s claim read (today: hardcoded `origin/master`, `ROOT` resolving into `src/`, and a second interpreter re-exec that dominates its 0.7 s).
8. Extend `mutation_manifest.rs` to own `mutation_campaign_generate plan`.
9. Last: `candidate_review` checks/engine/verification.
Exit criterion: per-tool-call fixed cost < 5 ms, measured by 0.1.

## Phase 2: bet B1, cost ledger (Claude opus design, Rust build)
Rust JSONL reader over harness transcripts: tokens by category (cache read/write, output), resident-context attribution per turn (which attachment, which tool result, which reminder), per-session and per-agent totals, a budget ratchet in the gate (fail a PR that regresses median hook ms or resend bytes/session). Model-routing policy at the harness seam (cheap tier for Explore/clerical) follows from the ledger's numbers.

## Phase 3: bet B2, correct incremental verification (Claude)
Transitive closure index (Rust) over imports and fixtures, per-test timing DB, flake ledger from the 1,494 receipts, content-addressed check cache keyed per file not per tree. Bound or de-scope the equivalence probe. Exit: gate time proportional to the diff, not the repo.

## Phase 4: bet B3, sandbox runner (Claude, after Tim's design nod)
Landlock/seccomp runner with write scope bound to the session's claims; replaces command-string heuristics as the safety floor.

## Do not build
Another duplication detector (four ship); changed-line coverage (exists); another repo map (code-review-graph covers it); LLM-as-judge as a blocking gate; aggressive context compression (measured to raise cost 6.8 % and halve patch success); Bazel/Nix; a fifth mutation engine.

## Ownership rule
Claude agents (≤150K each): anything Rust, anything that needs a design decision. GLM sessions: campaigns, config plumbing with a known pattern, docs generated from code, CI wiring, CI-failure fixes.

## Mutation platform fixes landed 2026-09-13 (GLM slice E)

- PR #25 ff1d7d8: `refresh` accepts campaigns admitted with extra tests (no more `--force` ratchet resets); `snapshot_worktree` keeps every tracked file under `tests/`, `fixtures/` and `test_*` paths plus `[tool.conductor].snapshot_extra_suffixes`; the generator pairs tests by package-relative path, not basename.
- PR #26 3e536f7: `campaigns/registry.d/<campaign-id>.json`, one file per row; `registry.json` is an empty read-only envelope; `conductor.mutation_registry_split` is the one-shot migration. Concurrent PRs no longer conflict on the registry.
- Open: cargo-mutants TIMED_OUT mutants still turn a campaign ERROR (debt in PR bodies); re-runs of campaigns that mutate other lanes' files need `--base` pinned to the pre-lane commit.


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
