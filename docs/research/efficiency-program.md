# llm-forge — landscape, research program and backlog (2026-11-05)

Reviewed at `d891e83` (#201); committed on top of `0a5ca5c` (#202).
Written for a local Claude Code / Codex session to pick up: items have IDs, files and
a "done when" check. Suggested home: `docs/research/efficiency-program.md`, with the
backlog items split into GitHub issues.

**Primary user:** the owner, across his own repos. Outside adoption is welcome but
secondary, which is why marketing is an optional appendix.

**Scope of Forge (as the owner defines it):** Forge is a *supporting tool layer* for
any AI coding app (Claude Code, Codex, Cursor, Aider, local-model CLIs). It is not
a coding CLI. It attacks four problems:

1. **Large codebases** — reading whole files exhausts the model's context.
2. **AI slop** — duplicated helpers, dead code, empty error handling, low-value tests.
3. **Bad persisted memory** — agents forget, or remember stale and unverifiable things.
4. **Long, slow code** — needlessly verbose and poorly performing output.

Plus two enabling layers that make the four trustworthy: **safe edits/execution** and
**measurement/receipts**.

---

## Part 1 — Landscape: which free tool covers which space

Each tool below was checked against its own README or a published write-up. Licences
are as reported by those sources. "Forge module" names the code in this repo that
covers the same space.

### Space 1 · Large codebases and context budget

| Tool | Licence / cost | What it does in this space | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `code_inspect`, `code_fold`, `code_search` (BM25 / hybrid / HNSW `vector_index`), `forge graph` (caller/import graph in `.forge/graph.db`), `context_count/compact/recover`, `context_prompt`, `tool_quiet` | Apache-2.0, free | Signature/folded views with source hashes; graph with explicit staleness; exact-BPE compaction with SHA-256-recoverable originals | — |
| **Serena** (oraios) | MIT, free | LSP-backed symbol tools for 40+ languages: overview, find symbol, references, symbol-level edits | No context compaction, no recovery, no source-hash freshness, no receipts |
| **Claude Context** (Zilliz) | MIT, free (needs vector DB + embedding API) | Hybrid vector code search over MCP; their 30-task SWE-bench A/B showed ~39% fewer tokens at equal retrieval F1 | Search only; depends on external vector DB/embeddings |
| **code-review-graph** | Open source, free | Tree-sitter → SQLite graph; "blast radius" of a change for reviews; 8.2× average token reduction, below 1× on small packages | Review-focused; no edits, compaction or memory |
| **Codebase Memory MCP** (DeusData) | MIT, free | Tree-sitter knowledge graph, 158 languages, structural queries | Graph/query only; its 99% claim has no published method |
| **Aider repo-map** | Apache-2.0, free (inside Aider only) | Ranked map of symbols, ~8–15k tokens for FastAPI's 108k lines | Locked to the Aider CLI |
| **Repomix** | MIT, free | Packs a whole repo into one file (~800k tokens for FastAPI) | Opposite approach: packs everything instead of selecting |
| **rtk** | MIT, free, Rust | Shell proxy compressing command output (git, tests, ls) 70–92% per command | Bash output only; built-in Read/Grep bypass it |
| **LLMLingua** (Microsoft) | MIT, free | Model-based prompt compression | Lossy, needs a small model, no recovery of originals |
| Anthropic Tool Search / code-execution pattern | Platform feature | Defers tool definitions; filters data in a sandbox (150k→2k tokens in their example) | Client-side technique, not a tool you install |

### Space 2 · AI slop and code quality

| Tool | Licence / cost | What it does | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `native/slop-core` (`detector_scan`, `verbosity_scan`, `style_scan`, `test_value_scan`, `hot_loop_scan`, `consolidation`, `gate`), `duplicate_bodies` / `duplicate_fingerprints`, `dead_tests`, reuse audit, radon/vulture baselines, `forge candidate` review gate, equivalence probe, mutation engines (cargo-mutants, Mull, fest, generated) | Apache-2.0, free | Scans, gates and *evidence* (mutation receipts) in one native pipeline | — |
| **AISlop** (scanaislop) | Open source | Hook-based detector for empty catches, useless comments, duplicated helpers, dead code | Pattern detector; no mutation evidence, equivalence or gate |
| **antislop** (Rust crate), **slopscore**, **aposlop** | Open source | Small slop-pattern scorers | Single-purpose scorers |
| jscpd / PMD-CPD, vulture, radon, clippy, ruff, Semgrep CE | Free | General duplication, dead-code, complexity and lint tools (Forge already consumes several as baselines) | Not agent-aware: no hooks, no per-edit gating |
| cargo-mutants, mutmut, Stryker, Mull | Free | Mutation testing per ecosystem | Forge orchestrates these; standalone they produce no agent-facing receipt |

### Space 3 · Persisted memory

| Tool | Licence / cost | What it does | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `notes_index`, `memory_ingest` / `memory_store` / `memory_catalog`, `workspace_recall` (immutable snapshots, caller-supplied embeddings), `embedding_broker`, `session_brief`, claims/inbox (`a2a_store`) | Apache-2.0, free | Memory bound to source hashes and snapshots; recall grants no authority; freshness is explicit | — |
| **Basic Memory** | MIT, free | Markdown notes on disk plus a semantic graph; human-editable | No source binding: notes can silently go stale against code |
| **Beads** | MIT, free | Issue/dependency graph versioned in the repo, so agents resume multi-step work | Task memory, not code/knowledge memory |
| **Knowledge Graph Memory** (official MCP reference server) | MIT, free | Entities/relations/observations in a local JSON file | Minimal, no retrieval ranking or freshness |
| **OpenMemory MCP** (mem0) | Open source core | Vector memory shared across clients | General chat memory; no code provenance |

No published recall/quality benchmark exists for any of these, Forge included. That
is a research opening (see R-MEM below).

### Space 4 · Long, slow code (verbosity and performance)

| Tool | Licence / cost | What it does | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `verbosity_scan`, `hot_loop_scan`, `complexity_cli`, `performance` receipts, `candidate_benchmark*`, criterion benches | Apache-2.0, free | Detects verbose/hot-loop patterns; sealed paired benchmark receipts | Not yet exposed as an in-loop "your edit made X slower" signal |
| **Codeflash** | **BSL-1.1, needs a codeflash.ai API key** — source-available, not free OSS | LLM proposes Python optimizations, verifies correctness, benchmarks, opens PRs | Python-focused, hosted LLM dependency |
| criterion, hyperfine, flamegraph, py-spy, clippy perf lints, ruff `PERF` rules | Free | Generic benchmarking/profiling/lints | Not wired to agents; no per-edit verdict |

### Enabling layer A · Safe edits and execution

| Tool | Licence / cost | What it does | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `code_replace` / `code_diff` / `code_restore` (hash-checked, Git recovery), persistent PTYs (`terminal_*`), operator policy + systemd confinement (`harness_policy`) | Apache-2.0, free | Edits fail if the file changed; every apply is recoverable; model cannot widen paths/programs | Linux + systemd only |
| **Desktop Commander MCP** | MIT, free | Terminal sessions plus file editing over MCP | No hash-checked edits, no confinement policy |
| **Anthropic sandbox-runtime** | Apache-2.0, free | OS-level sandbox (bubblewrap/seatbelt) used by Claude Code | Sandbox only, no edit or context tools |

### Enabling layer B · Measurement

| Tool | Licence / cost | What it does | Gap relative to Forge |
|---|---|---|---|
| **Forge**: `ledger/` (report, rollup, budget, calibrate), `context_telemetry`, sealed performance receipts | Apache-2.0, free | Source- and binary-bound receipts; missing evidence stays visible | Not yet reachable over MCP (REP items) |
| Langfuse / OpenLLMetry and similar | Open-source cores | LLM tracing and cost dashboards | Trace the model, not the tooling's effect on outcomes |

### What this landscape shows

**I found no free repository that covers more than one of these spaces in depth.**
Every tool above is single-purpose: search *or* graph *or* memory *or* slop patterns
*or* output compression. Forge covers all four problem spaces plus both enabling
layers in one native binary, with the same provenance model throughout: source
hashes, recoverable originals, receipts.

That breadth, plus being measurable, is the real differentiator. Correction to the
earlier draft: "crowded" was only true of the single sub-niche "MCP code search /
token reduction", where Serena, Claude Context and the graph tools compete. It is
not true of the space Forge occupies as a whole.

**On Rust:** the component data supports it. Secret scan went from 30.6 to 2.1 ms,
native scan from 19.4 to 1.9 ms, casefold 3–9×, and warm BM25 retrieval runs at
0.46 ms including source checks. The one slowdown in the docs, launcher relocation,
came from deliberate safety work: 768 fsyncs plus extra link and lock checks. It was
not caused by the language. DOC-11 makes that case explicitly.

---

## Part 2 — Research program: making *any* AI coding app more efficient

The goal is client-agnostic findings that Forge turns into tools. Every study uses
the existing receipt rules: source/binary identity, host, warmups, complete outcome
totals, and failed/censored runs kept visible.

### Shared infrastructure (build once)

| ID | P | Item | Done when |
|---|---|---|---|
| R-INF-01 | P0 | **Client-agnostic A/B runner.** Drives Claude Code (`-p --output-format json`), Codex exec, Aider and a local-model agent headlessly on the same task with Forge on/off. Records provider usage fields, tool calls, wall time and verified outcome. | `forge research run --client X --arm {baseline,forge} --tasks set.json` emits a sealed receipt per trial. |
| R-INF-02 | P0 | **Task sets.** (a) 30 SWE-bench Verified tasks, Python. (b) 20–30 tasks mined from your own Rust/C/Python repos with tests as verifiers. (c) a handful of Terminal-Bench-style shell tasks. | Task JSON with verifier commands, checked into `research/tasks/`. |
| R-INF-03 | P0 | **Paired statistics.** McNemar on paired pass/fail, bootstrap CIs on tokens and time, Benjamini–Hochberg across arms (the method of the 2026 harness-design study). | `forge research compare` outputs effect ± CI and significance, never a bare ratio. |
| R-INF-04 | P1 | **Transcript miner.** Parse Claude Code JSONL (`~/.claude/projects/`) and Codex logs to find full-file reads, repeated reads, largest tool results, and edit→fail→re-edit loops. | Weekly report of the top-20 waste patterns, each linked to a candidate Forge tool. |

### Space 1 · Large codebases

| ID | Research question | Method | Turns into |
|---|---|---|---|
| R-CTX-01 | At what repo size does inspect/fold/graph beat plain Read+Grep? | R-INF-01 across 4 repo sizes; plot tokens and pass-rate versus LOC | Published break-even curve; auto-enable advice |
| R-CTX-02 | How much does compaction help small-window and local models? | `context_compact` at 32k/64k/128k (the 2026 study found +35.7 pp at 32k vs +2.7 pp at 128k) | A "local-model mode" preset |
| R-CTX-03 | Does `context_recover` earn its keep? | Count recover calls; ablate. That study found recall tools were rarely used | Keep, auto-recover, or drop |
| R-CTX-04 | Server-side exploration: one call doing search→inspect→fold vs 3–5 agent calls | Code-execution pattern applied to code navigation | A batch `code_explore` tool |
| R-CTX-05 | Shell output: does a native rtk-style output compressor in `tool_quiet`/`post_tool` add value over rtk? | Same commands, both tools, tokens and missed-error rate | Integrate, or document using rtk alongside |

### Space 2 · AI slop

| ID | Research question | Method | Turns into |
|---|---|---|---|
| R-SLOP-01 | What slop do agents actually produce? Build a taxonomy with frequencies | Run slop-core over agent-authored diffs from R-INF-01 and your own history; label 200 findings by hand | Ranked detector priorities; a public dataset |
| R-SLOP-02 | Does an in-loop slop gate (post-edit hook) reduce slop without hurting pass rate? | A/B with the gate on/off | Default hook config |
| R-SLOP-03 | Precision of each detector | Hand-label samples; report precision per rule | Prune noisy rules; confidence levels in output |
| R-SLOP-04 | Mutation score as a test-quality signal for agent-written tests | Mutation score of agent tests vs human tests on the same functions | `test_value_scan` thresholds |

### Space 3 · Persisted memory

| ID | Research question | Method | Turns into |
|---|---|---|---|
| R-MEM-01 | Build a coding-memory benchmark: does memory help on session N+1? | Multi-session tasks where session 2 needs facts from session 1, some deliberately stale | First public benchmark in this space (none exist) |
| R-MEM-02 | Staleness: how often do remembered facts contradict current source? | Replay memory stores against later commits; Forge's source-hash binding vs Basic Memory / KG-memory | Headline: "stale-memory rate" |
| R-MEM-03 | What is worth remembering? | From transcripts (R-INF-04), find facts re-derived repeatedly across sessions | Auto-suggested memory entries |
| R-MEM-04 | Memory preload cost vs benefit | Tokens injected by `session_brief` vs improvement in pass rate | Default brief size |

### Space 4 · Long, slow code

| ID | Research question | Method | Turns into |
|---|---|---|---|
| R-PERF-01 | How much more verbose is agent code than the human fix for the same task? | LOC, cyclomatic complexity, token count of the patch vs SWE-bench gold patches | `verbosity_scan` thresholds; a "concise patch" signal |
| R-PERF-02 | Do agents introduce performance regressions? | Run criterion/pytest-benchmark on touched functions before and after agent patches | An in-loop `perf_delta` tool |
| R-PERF-03 | Does feedback fix it? | A/B: agent receives `perf_delta` + `verbosity` results after each edit vs not | Default post-edit feedback |
| R-PERF-04 | Rust/C hot loops: does `hot_loop_scan` predict real hotspots? | Correlate findings with flamegraph samples | Detector calibration |

### Tools this research should produce (feeds the backlog)

| ID | P | Tool | Space |
|---|---|---|---|
| T-01 | P0 | `quality_delta` — given a diff: slop findings, duplication, complexity, mutation-score delta | 2 |
| T-02 | P1 | `perf_delta` — benchmarks touched functions before and after; returns verdict and CI | 4 |
| T-03 | P1 | `code_explore` — server-side search→inspect→fold in one call | 1 |
| T-04 | P1 | `memory_check` — validates stored facts against current source hashes, flags stale ones | 3 |
| T-05 | P2 | `patch_conciseness` — compares patch size and complexity to the minimal diff that passes tests | 4 |

---

## Part 3 — Backlog: documentation, MCP, reporting

### Documentation

| ID | P | Item | Done when |
|---|---|---|---|
| DOC-01 | P0 | **Measurement drift.** `docs/benefits.md:88` says "Persistent MCP vs direct CLI — NOT YET MEASURED", but #189 and `research/performance/forge-program-20261104-mcp-transport-*.json` measured it. Audit every unmeasured row. | Each row links a receipt or stays unmeasured; a `make doc-claims-check` fails on stale references. |
| DOC-02 | P0 | **Separate user docs from the migration log.** 99 of 177 files in `docs/` are `native-*-migration.md` history. | Move them to `docs/internal/migration/`; `docs/README.md` opens with a 6-link user path. |
| DOC-03 | P0 | **README organised by the four problem spaces**, each with the Forge tool and one measured number, followed by the demo. Caveats stay, below the demo. | A newcomer sees "problem → Forge tool → evidence" in the first screen. |
| DOC-04 | P0 | Add `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`. | GitHub community checklist is green. |
| DOC-05 | P1 | **Client cookbook**: Claude Code, Codex, Cursor, VS Code, Zed, Aider, local-model CLIs, each with a "verify the catalog is visible" step. | `docs/clients.md` |
| DOC-06 | P1 | **Generated tool reference** from `harness_catalog.rs`, listing each tool's space, read-only/mutating status and typical output tokens. | `forge harness catalog --markdown` → `docs/mcp-tools.md`, diffed in local-check |
| DOC-07 | P1 | **"When Forge pays off"**, using R-CTX-01's break-even curve. | Published with its receipt. |
| DOC-08 | P1 | Put the roadmap's mermaid architecture diagram on the README. | — |
| DOC-09 | P2 | Glossary for `conductor`, Luna/Sol/Terra, KB ids and LLM-repo references. | External readers never need the monorepo. |
| DOC-10 | P2 | Platform statement on install line 1 (Linux/systemd/Rust 1.98), plus what works CLI-only elsewhere. | — |
| DOC-11 | P1 | **"Why Rust" page**: component speedups with receipts, and the relocation case explained (fsync/safety cost, with a profile showing where the time goes). | Linked from the README. |
| DOC-12 | P1 | **Landscape page** (Part 1 of this file), kept current. | `docs/landscape.md` |

### MCP server and tools

Current catalog: `code_inspect`, `code_fold`, `code_search`, `code_replace`,
`code_diff`, `code_restore`, `task_get`, `task_list`, `terminal_spawn`,
`terminal_write` (+ read/wait), `session_brief`, `workspace_recall`, `context_count`,
`context_compact`, `context_recover`, `context_prompt`, plus one optional outbound
tool. Protocol: modern `2026-07-28` plus three legacy versions. Capabilities
advertise **tools only**.

| ID | P | Item | Done when |
|---|---|---|---|
| MCP-01 | P0 | **Tool annotations** (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`). None exist today, so clients can't auto-approve safe calls. | Every tool annotated; a test enforces it. |
| MCP-02 | P0 | **`outputSchema`** for every tool. `structuredContent` is already emitted (`harness_mcp_protocol.rs:313`) without a declared schema. | Responses are validated against their schemas in tests. |
| MCP-03 | P0 | **Catalog token cost measured and budgeted.** | README states N tokens; local-check fails on more than 10% growth without a receipt. |
| MCP-04 | P1 | **Descriptions say when to call each tool**, not only what it guarantees. | Tool-selection rate improves in R-INF-01. |
| MCP-05 | P1 | **Server `instructions` as a 5-line playbook** (inspect before read; diff, don't rewrite; compact above X). | Measured selection-rate change. |
| MCP-06 | P1 | **Resources** for reports, **prompts** for workflows. Avoid Roots/Sampling/Logging, which `2026-07-28` deprecates. | `resources/list` and `prompts/list` populated. |
| MCP-07 | P1 | **Tasks extension** for long jobs such as index builds (23.6 min observed); align `task_get/task_list` with it. | Long calls return task handles. |
| MCP-08 | P1 | Ship T-01…T-04 from Part 2 as MCP tools. | — |
| MCP-09 | P2 | Optional Streamable HTTP transport behind policy, for remote/devcontainer agents. | Off by default. |
| MCP-10 | P2 | Conformance tests using the official TS/Python SDK clients. | Added to local-check. |

### Reporting through MCP

The ledger, telemetry, candidate reports and receipts are rich, but none is
reachable by the agent or user over MCP mid-session.

| ID | P | Item | Done when |
|---|---|---|---|
| REP-01 | P0 | `session_report` tool/resource: tokens returned, estimated tokens avoided vs full-file baseline, compaction ratio, recoveries, slop findings, p50/p95 latency. | "What did Forge do this session?" gets a receipt-backed answer. |
| REP-02 | P0 | Per-call `_meta`: `{returned_tokens, baseline_tokens, baseline_kind}`. | Every call becomes a data point for Part 2. |
| REP-03 | P1 | Ledger rollup as a read-only resource (`forge://ledger/rollup?since=…`). | Matches `forge ledger report`. |
| REP-04 | P1 | Tool-selection and recover-rate logging from hooks. | Feeds R-CTX-03 and MCP-04. |
| REP-05 | P1 | Optional OpenTelemetry export (GenAI semantic conventions). | Off by default. |
| REP-06 | P2 | `forge report html`: a static, shareable report built from the ledger. | One self-contained file. |

### Quick wins (this week)

1. DOC-01 drift fix.
2. MCP-01 annotations + test.
3. REP-02 per-call `_meta`, so data collection starts now.
4. DOC-04 standard files.
5. R-INF-04 transcript miner on your own sessions: free data, no new infrastructure.

---

## Part 4 — Novel ideas not covered by existing tools

Forge is primarily its owner's tool, so these are ranked by value to a single heavy
user across several repos (ARIA, Bionode, Forge itself) first, and by value to others
second. Each idea names the Forge building blocks it reuses and the cheapest first
experiment. I did not find any of these shipped as a free tool in Part 1's landscape.

### AI coding

| ID | Idea | Problem it attacks | Built from | First experiment |
|---|---|---|---|---|
| N-01 | **Subtractive pass (patch delta-debugging).** After an agent's patch passes tests, mechanically try deleting each added hunk, line or helper (ddmin) and keep only what the tests and equivalence probe require. The result is a minimal passing patch, plus a list of lines the tests never needed. | Slop, verbosity | mutation engines, `equivalence_probe`, candidate gate, Git recovery | Run on 50 past agent patches from your repos; report % of added lines removable with tests still green. |
| N-02 | **Hallucinated-symbol gate.** Before running anything, resolve every identifier a diff introduces against the graph and the dependency index. Invented functions, wrong arities and phantom imports fail instantly, without a build/test cycle. | Slop, wasted turns | `forge graph`, `syntax`, `candidate_import_declaration` | Count how many failed agent edits in transcripts were unresolved symbols. |
| N-03 | **Negative memory (tombstones).** Record approaches that failed (reverted, tests red, perf worse), bound to the source hashes they touched. Surface them when the agent heads the same way, and expire them automatically when that source changes. | Memory; agents retrying dead ends | ledger outcomes, `memory_store`, source hashes, `session_brief` | Transcript-mine repeated failed approaches within and across sessions. |
| N-04 | **Executable memory.** A remembered fact ("X is always sorted", "this endpoint returns ≤100 rows") is stored with an invariant probe that re-verifies it. Memory that can't be verified is labelled unverified; memory that fails is retired. | Stale memory | `invariant_probe`, `memory_store`, `test_contracts` | Convert 20 facts from your notes into probes; measure the share that is already false. |
| N-05 | **Agent-comprehension cost as a code-quality metric.** Tokens needed to understand a function = signature + folded callees + types it touches. Flag functions above a budget as refactor targets. Combine with telemetry: symbols agents re-read most often are your hardest code. | Large codebases | `code_fold`, graph, `context_count`, `context_telemetry` | Rank symbols in ARIA by comprehension cost and re-read frequency; check them against your intuition. |
| N-06 | **Prompt-cache-aware context layout.** Order and freeze context (briefs, folded files, rules) so the provider's cached prefix survives between turns, and report cache-hit rate. Deterministic output is already a Forge property; this exploits it. | Cost and latency | `context_compact` determinism, `session_brief`, ledger | Compare cache-read tokens in provider usage with stable vs default ordering. |
| N-07 | **Session passport for cross-client handoff.** A portable record of task, files read (with hashes), decisions, tombstones and open checks. Switching Claude → Codex → local model resumes without re-reading; anything whose hash changed is re-read. | Memory, context | `task_*`, journal, source hashes, `context_recover` | Hand a half-done task between two clients; count re-read tokens with and without the passport. |
| N-08 | **Blast-radius edit gate.** Block or warn when an agent edits a symbol without having inspected its current callers (from the graph) in this session. Read receipts become a precondition for edits. | Large codebases, regressions | graph, `code_replace` hash checks, hook adapters | Measure how many regressions in R-INF-01 touched un-inspected callers. |
| N-09 | **Per-model scorecards on your own code.** Attribute every finding (slop, mutation survivors, perf deltas, reverts) to the model and client that wrote the lines, via commit trailers and the ledger. Choose models per task type from your own data, not public leaderboards. | All four spaces | mutation attribution, ledger `agent`/`outcome`, routing policy | Backfill attribution on the last month of commits. |
| N-10 | **Budget contracts.** The agent declares a token/turn budget per task. Forge tracks spend and, near the limit, returns a targeted suggestion ("3 full-file reads of X; `code_fold` would cost ~400 tokens"). | Context | `ledger/budget.rs`, REP-02 `_meta` | Run with soft warnings; measure overrun rate. |
| N-11 | **Speculative pre-folding.** Predict the next files from the graph plus learned navigation patterns, and pre-compute folded views so the next inspect is instant and pre-counted. | Latency | graph, `code_fold`, transcript miner | Check offline prediction accuracy on transcripts first. |

### Documentation

| ID | Idea | Problem it attacks | Built from | First experiment |
|---|---|---|---|---|
| N-D1 | **Hash-bound docs.** Each doc section declares the symbols it describes. When their source hash changes, the section is flagged stale in local-check. This generalises your receipt discipline to every repo's prose. | Doc drift | source hashes, graph, `local_check` | Annotate `docs/native-harness.md`; count flags over a week of commits. |
| N-D2 | **Agent cards.** Per module, a generated ≤300-token card (purpose, entry points, invariants, gotchas, tombstones), hash-bound and served by `session_brief` on demand. Replaces giant AGENTS.md/CLAUDE.md preloads. | Context, memory | notes index, graph, `session_brief` | Measure preload tokens and task pass rate: one big AGENTS.md vs cards. |
| N-D3 | **Docs from confusion.** The transcript miner flags where agents searched and missed, misread an API, or asked the user. Each cluster becomes a proposed doc entry or agent-card gotcha. | Docs that answer real questions | R-INF-04, `code_search` miss logs | Top-10 confusion clusters from a month of your sessions. |
| N-D4 | **Executable docs everywhere.** Every shell/code block in Markdown is extracted and run in the confined harness during local-check, with expected output or a hash. README commands can no longer rot. | Doc correctness | harness, `terminal_*`, `local_check` | Run against Forge's own README and install docs. |
| N-D5 | **Claim checker for prose.** Any number in docs ("84.4% fewer tokens") must reference a receipt ID. Unreferenced or superseded numbers fail the check. This is DOC-01 as a reusable tool for any repo. | Doc honesty | performance receipts, ledger | Run on Forge docs; DOC-01's drift would have been caught. |
| N-D6 | **Instruction-file pruning.** Measure which AGENTS.md/CLAUDE.md rules are ever exercised (hook events, violations, transcript references) and what each costs in preload tokens. Propose deleting dead rules. | Context | `context_count`, hooks, transcript miner | Per-rule usage over a month for your repos. |

### Suggested first three (personal value, low cost)

1. **N-02 hallucinated-symbol gate.** Small, uses the existing graph, saves a turn on every bad edit.
2. **N-01 subtractive pass.** Directly attacks verbose slop with machinery you already trust.
3. **N-03 + N-04 tombstones and executable memory.** A genuinely different take on memory: verifiable and expiring instead of a bigger note pile.

---

## Appendix (optional) — Marketing

Entirely optional; nothing above depends on it. If you choose to promote Forge:

- **Lead with breadth plus evidence.** "Free, local, native tools for any AI coding
  app: read less, catch slop, remember with provenance, prove what ran." Use Part 1
  as the comparison page.
- **Publish research, not slogans.** Part 2 findings (break-even curve, slop
  taxonomy, stale-memory rate, a coding-memory benchmark) are publishable on their
  own. Post write-ups to dev.to or a blog, then Show HN, r/LocalLLaMA, r/ClaudeAI,
  r/rust, Lobsters.
- **Lower the try-it cost.** Prebuilt Linux binaries on GitHub Releases or
  `cargo binstall`, an inspect-only quickstart with no harness, a devcontainer.
- **Discovery.** Official MCP Registry (`server.json` + `mcp-publisher`,
  `io.github.mcpirate17` namespace), GitHub topics and description, a 30-second
  demo GIF, tagged releases, Claude Code plugin packaging.
- **Security audience.** A short threat-model page (what is confined, what isn't)
  suits AppSec/OT security meetups.

---

## Sources

- [Serena MCP guide](https://mcp.directory/blog/serena-mcp-complete-guide-2026)
- [Claude Context evaluation](https://glama.ai/mcp/servers/@zilliztech/claude-context/blob/2484ae29b3a9c92815f1524cbb9924b3e241ec17/evaluation/README.md)
- [code-review-graph analysis](https://dev.to/emperorakashi20/how-code-review-graph-cuts-claude-code-token-usage-by-49x-and-whether-its-actually-worth-it-4kn1)
- [Codebase Memory MCP](https://landscape.jimmysong.io/projects/codebase-memory-mcp/)
- [Repomix vs Aider repo-map vs Codebase Memory on FastAPI](https://dev.to/thegdsks/i-tested-4-codebase-to-ai-tools-on-fastapi-108k-lines-here-are-the-token-costs-4bmc)
- [rtk write-up](https://themenonlab.blog/blog/rtk-cli-proxy-token-reduction-ai-agents)
- [LLMLingua](https://www.sourcepulse.org/projects/1237038)
- [AISlop](https://news.ainauten.com/en/story/aislop-ein-cli-gegen-ai-generierte-code-smells)
- [antislop](https://docs.rs/crate/antislop/latest/source/README.md)
- [slopscore](https://pypi.org/project/slopscore/)
- [Memory MCP servers 2026](https://mcp.directory/blog/claude-code-memory-mcp-servers-2026)
- [Codeflash](https://github.com/codeflash-ai/codeflash)
- [Desktop Commander MCP](https://pyshine.com/DesktopCommanderMCP-Terminal-Filesystem-Control-for-Claude/)
- [Anthropic sandbox runtime](https://aiweekly.co/alerts/anthropic-open-sources-claude-code-sandbox-under-apache-20)
- [Anthropic: code execution with MCP](https://www.anthropic.com/news/code-execution-with-mcp)
- [MCP context bloat fixes 2026](https://mcp.directory/blog/mcp-context-bloat-fix-2026-tool-search-code-mode-progressive-disclosure)
- [Empirical study of harness design for coding agents](https://arxiv.org/pdf/2609.20804)
- [MCP spec update July 2026](https://www.scalekit.com/blog/mcp-spec-update-july-2026)
- [MCP registries guide](https://roxyapi.com/blogs/mcp-registries-where-to-list-your-server-2026)
