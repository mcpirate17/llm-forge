# Forge efficiency program and canonical backlog

Reconciled on 2026-11-06 against source `2e89322` and the delivered contracts,
implementation and tests at that revision. The original proposal was reviewed at
`d891e83` (#201), based on `0a5ca5c` (#202), and preserved in commit
`96337e3980b3a38981b31abd3d753c1a4feea87d`. Later work through #231 is considered
where its behavior is present in the compared tree. This is a documentation
reconciliation; no new experiment or client run was performed.

**This is the single ordered improvement/research backlog.** It merges the original
71 stable IDs with the remaining program in [the roadmap](../roadmap.md) and
[the migration status](../native-migration-evidence-status.md). The roadmap retains
architecture and delivery history. Known defects belong in the
[GitHub issue tracker](https://github.com/mcpirate17/llm-forge/issues), not a second
local defect ledger. Entries here describe scope, evidence and acceptance; issue
status remains authoritative. Historical migration task lists are leads to verify,
not proof that their named callers remain unmigrated.

Forge supports existing coding clients; it is not a coding CLI. Its primary user
is its owner across his repositories. The four research spaces remain:

1. **Large codebases:** reduce irrelevant reads and context pressure.
2. **AI slop:** reduce duplication, dead code, weak tests and unnecessary machinery.
3. **Persisted memory:** retain useful facts with provenance and explicit freshness.
4. **Long, slow code:** reduce needless verbosity and measured runtime regressions.

Safe edits/execution and trustworthy measurement enable all four. Native code,
installation, runtime activation, verified client use and demonstrated task benefit
are separate milestones. Preserve novel non-QKV mechanisms and matched controls
in host research; a failed result closes a measured gap rather than authorizing a
substitute architecture.

## Landscape: historical research leads, not current competitive claims

The original landscape compared Forge with the tools below. Its third-party
licenses, capabilities, protocol forecasts, novelty claims and quantitative results
have **not been independently reverified in this reconciliation**. The dated links
at the end remain research leads. Do not use “first”, “only”, “no benchmark exists”,
“free”, or comparative percentages in product copy without a new primary-source
review. The original study's model-window percentages and protocol-deprecation
assertions are hypotheses to check, not premises for implementation.

| Space | Existing Forge surfaces to evaluate | Original comparison leads |
|---|---|---|
| Large codebases | `code_inspect`, `code_fold`, `code_search`, graph, exact tokenizer counting, compaction/recovery, quiet output | Serena, Claude Context, code-review-graph, Codebase Memory MCP, Aider repo-map, Repomix, rtk, LLMLingua, Anthropic tool-search/code-execution patterns |
| Slop and quality | Slop Core detectors, duplicate/dead-test analysis, candidate gates, equivalence probes and automatic mutation engines | AISlop, antislop, slopscore, aposlop, jscpd/PMD-CPD, vulture, radon, clippy, ruff, Semgrep CE, cargo-mutants, mutmut, Stryker, Mull |
| Persisted memory | Source-bound memory ingestion/store, immutable recall snapshots, notes/cards, session briefs and workspace observations | Basic Memory, Beads, reference Knowledge Graph Memory, OpenMemory/mem0 |
| Verbosity/performance | Verbosity/hot-loop scans, complexity analysis, paired benchmark receipts and criterion benches | Codeflash, criterion, hyperfine, flamegraph, py-spy, clippy performance lints, ruff PERF |
| Safe edits/execution | Hash-checked replace/diff/restore, durable terminals/tasks, operator policy and confinement | Desktop Commander MCP, Anthropic sandbox runtime |
| Measurement | Ledger, telemetry, sealed source/binary-bound performance evidence | Langfuse, OpenLLMetry and related tracing systems |

These are overlapping building blocks, not evidence that proposed composite tools
or studies below have shipped. [Benefits](../benefits.md) and
[performance](../performance.md) retain scoped numbers, comparators and limits.
For example, native reporting, retrieval and transport measurements cannot establish
faster agent tasks or smaller provider bills. The native relocation slowdown stays
visible alongside component improvements.

## What later work actually delivered

| Delivered slice | Code/test/contract evidence | Boundary that remains |
|---|---|---|
| Reviewed MCP read annotations | `harness_catalog.rs::source_read_tool`; `harness_catalog_tests.rs::only_reviewed_source_reads_advertise_effect_annotations`; protocol discovery tests | Only `code_inspect` and `code_fold` carry reviewed effects. Other tools are deliberately unclassified; MCP-01 remains partial. |
| Confined Cargo workspace/selected-test mutation, progress after timeout, explicit Fest and standalone Mull profiles | [Mutation contracts](../mutation.md), [Fest](../mutation-fest.md), [Mull](../mutation-mull.md), `mutation_cargo_supporting` tests and [performance evidence](../performance.md#cargo-workspace-mutation-evidence--2026-11-06) | Scoped automatic controls are not full-package PASS, all profile support, agent-versus-human test quality, or the subtractive pass. See CF-01 and R-SLOP-04. |
| Native mutation evidence/coverage/canary and Python test-import reports | `native/forge/src/mutation_evidence_cli.rs`, `mutation_evidence_inventory.rs`, `test_report.rs`; [analysis contract](../native-analysis.md#python-test-import-report); focused report and CLI tests | These reports are not a combined diff-level `quality_delta`, session report or conservative affected-test verifier. |
| Policy-enabled `python_test_report` over MCP | `harness_catalog.rs`, protocol cancellation tests, `native/forge/tests/native_mcp_setup.rs`; [confined report evidence](../performance.md#confined-mcp-test-report--2026-11-06) | One advisory report uses a confined worker per request. Its measured service cost and lifecycle controls do not establish real-client adoption or agent benefit. |
| Native backlog fold/publication and equivalence gate extensions | [Backlog contract](../equivalence-backlog.md), [gate contract](../equivalence-gate.md); `equivalence_backlog_cli.rs`, `equivalence_gate_cli.rs`, `equivalence_gate_live.rs` | Artifact folding is not this planning backlog. Equivalence and safe publication are reusable machinery, not N-01 minimal-patch search or N-04 executable memory. |
| Transport, prompt-contract and rejected recommendation measurements | [Transport/task evidence](../performance.md#transport-task-value-and-missing-controls), [rejected next-inspection advice](../performance.md#deferred-next-inspection-advice--2026-11-05) | Matched microbenchmarks exist. First actual Codex probe was approval-blocked; automatic recommendation was rejected and removed. Neither counts as successful adoption. |

Code/test names above are under `native/forge/src/` and `native/forge/tests/`
unless stated otherwise. Tests are implementation evidence inspected here; their
historical execution results remain bound to the revisions in linked receipts.

## Ordered implementation backlog

Order is by dependency and value to actual user work: repair evidence clarity,
start useful telemetry, prove one client/workload, then choose features from measured
gaps. P0/P1/P2 are the proposal's priorities, not execution approval or deadlines.
No multi-hour/model run, external-client approval change or promotion is authorized
by an entry in this document.

**Status:** implemented = original acceptance met by an equivalent shipped surface;
partial = a substantive subset exists but acceptance remains; open = proposed
capability/deliverable absent; evaluation-needed = a research question or experiment
still lacks the stated evidence; deferred = deliberately conditional or lower priority.
Existing primitives alone do not make a new composite tool partial.

### 1. Evidence clarity, safe discovery and data collection

| ID | P / status | Scope and current evidence | Remaining acceptance / done when |
|---|---|---|---|
| DOC-01 | P0 / partial | Audit measurement drift. `benefits.md` still labels MCP versus CLI unmeasured, while [transport receipts](../../research/performance/forge-program-20261104-mcp-transport-baseline.json) and the performance report cover scoped comparisons. The stale row is tracked in [#232](https://github.com/mcpirate17/llm-forge/issues/232). | Link each row to its exact receipt and scope, keep actual-client/task effects unmeasured, and add a native stale-reference check only if its scope is justified (`make doc-claims-check` was originally proposed). Coordinate the repair through #232. |
| REP-02 | P0 / open | Per-call `{returned_tokens, baseline_tokens, baseline_kind}` metadata. `harness_mcp_protocol.rs::success` currently emits server identity in modern `_meta`; it does not emit these metrics. | Every eligible call exposes calibrated counts with declared baseline semantics and missing values; tests cover successful/error/legacy behavior. Starts data collection for R-INF and REP-01. |
| MCP-01 | P0 / partial | Four boolean effect annotations exist only on reviewed `code_inspect`/`code_fold`; catalog tests explicitly require others absent. | Review each remaining tool's real effects and annotate only justified values; test complete reviewed coverage without expanding approval authority. Hints alone cannot promise client auto-approval. |
| MCP-02 | P0 / open | Own catalog publishes input schemas; protocol emits structured content. Outbound `outputSchema` validation is a separate client-side capability. | Declare each inbound tool's output schema and validate successful structured responses against it, with explicit error/legacy handling. |
| MCP-03 | P0 / open | Catalog/tokenizer machinery exists, but no catalog growth budget was found. | Publish catalog token cost for a pinned tokenizer and declared capability profiles; require a receipt for >10% growth in local-check. |
| DOC-04 | P0 / open | Root `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md` are absent in the compared tree. | Add maintained contribution/security/release guidance aligned with AGENTS.md; verify links and GitHub community surfaces. |
| DOC-02 | P0 / open | User entry points and migration history remain mixed; `docs/README.md` remains chiefly a platform/KB reference. | Create a six-link user path and move migration history into an internal archive with working redirects/links. The original 99/177 count is historical, not a current inventory. |
| DOC-03 | P0 / partial | README already leads with user jobs, a demo and bounded evidence, but not the original four-space presentation. | Make all four problem spaces discoverable with actual tools and scoped evidence. Use one measured number only where justified; do not invent one per space. |
| R-INF-04 | P1 / partial | Transcript miner for repeated/full-file reads, large tool results and edit→failure loops. `ledger/reader.rs`, read statistics, fixture tests and `ledger/report.rs` provide Claude transcript/telemetry parsing. | Add Codex ingestion and a reproducible top-20 weekly waste-pattern report linked to candidate tools; preserve malformed/missing input totals and privacy bounds. Reuse the ledger rather than build another parser. |
| REP-04 | P1 / partial | Hook telemetry and ledger read/usage summaries exist (`context_telemetry.rs`, ledger fixtures). Unified tool-selection/recover-rate analysis is not established. | Emit/join selection and recovery events with explicit denominators and missing logs; feed R-CTX-03 and MCP-04 without treating estimates as provider counts. |
| REP-01 | P0 / open | Proposed session report of returned/avoided tokens, compaction, recoveries, slop and p50/p95 latency. CLI ledger/report primitives exist; `python_test_report` is a different report. | Expose a read-only `session_report` tool/resource with receipt-backed fields, declared full-file baselines, estimated-versus-observed labels and unavailable metrics visible. |

### 2. Prove actual client and task value before broadening features

| ID | P / status | Scope and current evidence | Remaining acceptance / done when |
|---|---|---|---|
| DOC-05 | P1 / partial | Client setup fragments exist in README, `native-mcp-setup.md`, harness/provider docs. First Codex diagnostic reached discovery but calls were approval-blocked ([evidence](../performance.md#actual-codex-approval-diagnostic--2026-11-05)). | A client cookbook covers Claude Code, Codex, Cursor, VS Code, Zed, Aider and local CLIs, labeling tested versus suggested setup. First prove one actual client: catalog visible, agent-selected read, allowed/denied/error calls, reconnect and undo with narrowly authorized policy. |
| R-INF-01 | P0 / open | Client-agnostic paired runner for Claude Code JSON, Codex exec, Aider and a local agent. Existing benchmark/receipt infrastructure is not this runner. | A bounded `forge research run --client X --arm {baseline,forge} --tasks set.json` or equivalent emits a sealed trial with provider usage, tool calls, source/model/budget, verified outcome and wall time; retain provider usage, recoveries/rereads and failed/censored/unstarted trials. Start with one real client and owner's tasks. |
| R-INF-02 | P0 / open | Task sets: 30 SWE-bench Verified Python tasks, 20–30 owner-repo Rust/C/Python tasks, and a few shell/Terminal-Bench-style tasks. Existing detector/CLI fixtures are not agent tasks. | Check in task manifests under `research/tasks/` with provenance, licenses where needed, independent verifiers and bounded resources. Begin with a small owner-workload pilot before expanding the proposed counts. |
| R-INF-03 | P0 / open | Paired pass/fail and cost/time statistics. Microbenchmark summaries do not supply this shared analysis. | `forge research compare` or equivalent reports McNemar paired outcomes, paired bootstrap CIs and declared multiple-comparison handling (originally Benjamini–Hochberg); preserve failed/censored/unstarted totals and never report a bare ratio as proof. |
| MCP-04 | P1 / evaluation-needed | Descriptions currently explain guarantees and some use cases; automatic next-inspection advice was measured, rejected and removed. | Improve when-to-call descriptions and measure tool selection plus task success with R-INF-01. Retain rejected controls and token overhead; no default recommendation without prospective value. |
| MCP-05 | P1 / partial | Protocol already returns safety/reconnect `instructions`; it is not the proposed five-line navigation/compaction playbook. | Test a concise inspect/diff/compact playbook against current instructions; measure selection and outcomes, choose thresholds from data, and retain authority warnings. |
| MCP-10 | P2 / open | Custom native wire clients and protocol tests exist, not official TS/Python SDK conformance or actual-client success. | Exercise supported protocol versions using pinned official clients in local-check; keep this distinct from DOC-05 actual coding-client adoption. Separate cold service/adapter startup and discovery from warm dispatch in matched CLI/MCP comparisons. Verify the specification before changing protocol behavior. |
| DOC-07 | P1 / evaluation-needed | “When Forge pays off” depends on R-CTX-01, not existing single-file token reductions. | Publish break-even advice with paired task outcomes, preparation/setup cost and its receipt. |

### 3. Research across the four spaces

All studies use source/binary/model identity, declared host/resources and warmups,
matched controls, complete outcomes and uncertainty. Historical competitor/study
numbers do not substitute for these experiments.

| ID | Space / status | Question and method | Remaining acceptance / done when |
|---|---|---|---|
| R-CTX-01 | 1 / evaluation-needed | Across four repository sizes, compare inspect/fold/graph with plain Read+Grep under R-INF-01. | Plot tokens, wall time and verified pass rate against size; include graph/index preparation; publish a break-even curve and optional auto-enable advice. |
| R-CTX-02 | 1 / evaluation-needed | Ablate compaction at 32k/64k/128k for small-window/local models. Compaction implementation alone does not answer this. | Measure quality/cost at each window and choose a local-model preset only if supported. Recheck the original harness-study figures before citing them. |
| R-CTX-03 | 1 / evaluation-needed | Count and ablate recovery calls, including omissions/repeated reads. Recovery machinery exists. | Decide keep/auto-recover/drop from matched outcomes and recover-rate evidence, preserving correctness and recoverability until a replacement is proved. |
| R-CTX-04 | 1 / evaluation-needed | Compare server-side search→inspect→fold with 3–5 separate calls. | Quantify calls/tokens/latency and verified outcomes; use the result to decide T-03 rather than assume batching helps. |
| R-CTX-05 | 1 / evaluation-needed | Compare native quiet/post-tool output compression with rtk on identical commands. | Report returned tokens, missed errors and total cost; choose integration or documented coexistence. No unverified competitor percentage is an acceptance target. |
| R-SLOP-01 | 2 / evaluation-needed | Run detectors on agent-authored diffs from R-INF-01 and owner's history; hand-label 200 findings. | Publish taxonomy, frequencies and labeled provenance, including sampling uncertainty; rank detector work from actual incidence. |
| R-SLOP-02 | 2 / evaluation-needed | A/B a post-edit slop gate against no gate. Existing candidate gates are infrastructure. | Show slop reduction, false positives, pass rate and task cost before choosing default hook behavior. |
| R-SLOP-03 | 2 / evaluation-needed | Hand-label samples for each detector. | Report per-rule precision/confidence and prevalence; prune or downgrade noisy rules. Existing test assertions are not a precision study. |
| R-SLOP-04 | 2 / evaluation-needed | Compare automatic mutation scores for agent/human tests on the same functions. Cargo/Fest/Mull controls establish engine semantics, not this comparison. | Produce matched subject/test/resource accounting and complete engine outcomes; calibrate `test_value_scan` thresholds without calling survivors/timeouts PASS. |
| R-MEM-01 | 3 / evaluation-needed | Multi-session tasks where session N+1 needs earlier facts, including deliberately stale facts. | Publish a reproducible coding-memory benchmark and quality/cost results. Do not claim it is the first or that competitors lack benchmarks without a new review. |
| R-MEM-02 | 3 / evaluation-needed | Replay remembered facts against later commits; compare source-bound recall with selected alternative memory stores. | Define and measure stale/contradictory-memory rate with a labeled oracle, source identities and explicit unsupported fact types. |
| R-MEM-03 | 3 / evaluation-needed | Mine transcripts for facts repeatedly re-derived across sessions. | Validate useful memory suggestions against false positives, freshness and operator control; no automatic authority or unrequested persistence. |
| R-MEM-04 | 3 / evaluation-needed | Compare `session_brief` preload tokens with task benefit. | Choose a default brief size from pass rate, rereads and cost, including stale/irrelevant brief content. |
| R-PERF-01 | 4 / evaluation-needed | Compare agent patches with human/SWE-bench gold fixes by LOC, complexity and tokens. | Calibrate verbosity/concise-patch signals without equating shorter code with correctness or equivalent behavior. |
| R-PERF-02 | 4 / evaluation-needed | Benchmark touched functions before/after agent patches using criterion/pytest-benchmark or equivalent. | Measure regression incidence with equivalent work and source-bound paired samples; decide T-02 scope. |
| R-PERF-03 | 4 / evaluation-needed | A/B post-edit performance/verbosity feedback. | Show whether feedback repairs regressions without reducing correctness or merely moving cost into measurement. |
| R-PERF-04 | 4 / evaluation-needed | Correlate Rust/C hot-loop findings with profiler samples. | Publish predictive precision/coverage and calibrate detectors on actual hotspots. |

### 4. Composite tools: proposals, not renamed existing primitives

| ID | P / status | Proposed tool and reuse | Remaining acceptance / done when |
|---|---|---|---|
| T-01 | P0 / open | `quality_delta`: diff-level slop, duplication, complexity and mutation-score delta. Reuse detectors, native reports and candidate/equivalence evidence. | One bounded report joins before/after results with compatible scopes and identities; partial/missing mutation evidence stays explicit. Existing test-import or backlog reports do not complete this item. |
| T-02 | P1 / open | `perf_delta`: paired benchmarks of touched functions, verdict and CI. Reuse sealed performance/candidate benchmark machinery. | Admit explicit benchmark mappings and resources; prove behavior parity and uncertainty, including slowdowns/failed samples, before suggesting default post-edit use. |
| T-03 | P1 / open | `code_explore`: one server-side search→inspect→fold call. Current three tools are separate catalog entries. | Ship bounded source-fresh results after R-CTX-04 shows value; compare selection/output overhead with separate calls. |
| T-04 | P1 / open | `memory_check`: stored-fact validation against current sources. `memory_store.rs` and ingestion verify captured source identities, not arbitrary fact truth against a changed repository. | Report fresh/stale/unverifiable facts with current bindings; test deliberate source drift and immutable-store behavior without implying semantic truth from a hash. |
| T-05 | P2 / open | `patch_conciseness`: compare patch size/complexity with a minimized passing diff; reuse N-01. | Return candidate reductions plus tested scope and equivalence limits; never call a locally minimal or under-tested patch globally minimal. |
| MCP-08 | P1 / open | Expose T-01…T-04 over MCP after their CLI/core contracts exist. | Policy-bounded schemas, provenance, failure semantics and actual-client controls for each delivered tool; neither existing scans nor `python_test_report` satisfy this umbrella. |

### 5. Remaining MCP/reporting and documentation surfaces

| ID | P / status | Scope and current evidence | Remaining acceptance / done when |
|---|---|---|---|
| REP-03 | P1 / open | Read-only ledger rollup resource, proposed `forge://ledger/rollup?since=…`. CLI `ledger/report.rs` and rollup exist; inbound capabilities remain tools only. | Match CLI report semantics, bounds and freshness through a policy-enabled resource; include missing/uncalibrated data. |
| MCP-06 | P1 / open | Resources for reports and prompts for workflows. Current protocol advertises tools only. | Populate `resources/list` and `prompts/list` with explicit access policy and tested current-spec behavior. Do not base implementation on the original unverified Roots/Sampling/Logging deprecation claim. |
| MCP-07 | P1 / partial | Durable `task_get`/`task_list` and terminal task records exist; they are not proof of MCP Tasks extension support. | Verify the extension/specification, return interoperable handles for bounded long jobs such as index builds, and test cancellation/reconnect/expiry; distinguish response suppression from worker cancellation or undo. |
| REP-05 | P1 / deferred | Optional OpenTelemetry export using verified current GenAI conventions. No such MCP/ledger export found. | Off by default, explicit data/privacy/resource policy, tested field mapping and bounded overhead. Select after core telemetry is useful. |
| MCP-09 | P2 / deferred | Optional Streamable HTTP for remote/devcontainer agents; current constrained stdio outbound slice is not HTTP. | Off by default; prove explicit endpoint/credentials/policy, lifecycle and cancellation after local real-client value is established. |
| REP-06 | P2 / open | Static `forge report html` built from ledger. | One self-contained shareable file with receipt links, uncertainty, missing evidence and bounded/private data selection. |
| DOC-06 | P1 / open | Generated MCP tool reference from `harness_catalog.rs`. Catalog exists; `docs/mcp-tools.md` and proposed markdown generation do not. | `forge harness catalog --markdown` or equivalent generates space/effect/output-size guidance from declared profiles; local-check detects drift and labels measured versus estimated token sizes. |
| DOC-08 | P1 / open | Put roadmap architecture diagram on README; the diagram currently lives in roadmap. | Add a concise diagram with working links if it improves the user path; keep one source of truth. |
| DOC-09 | P2 / partial | `docs/README.md` explains KB anchors, `conductor` and host-policy separation; a complete reader glossary is absent. | Define conductor, model-role names, KB IDs and legacy LLM-host references without requiring external monorepo knowledge. |
| DOC-10 | P2 / partial | README install path states Linux/Rust 1.98; harness limitations are documented separately. | Make platform/prerequisite limits clear at installation entry and document what CLI-only operations are verified elsewhere. Do not imply every CLI operation requires systemd or claim untested portability. |
| DOC-11 | P1 / implemented | Equivalent “Why Rust” explanation is already in [benefits](../benefits.md#does-moving-the-implementation-to-rust-improve-performance), linked from README; [product-value measurements](../native-product-value-measurements.md) retain component receipts and relocation profiling. | Maintain scoped speedups, the slower relocation comparator and fsync/safety costs. A duplicate standalone page is unnecessary; new whole-task claims still require R-INF evidence. |
| DOC-12 | P1 / open | Original landscape is preserved as historical leads above, not a maintained current comparison. | Publish `docs/landscape.md` only after reviewing primary sources and marking review dates, scope and uncertain claims; avoid duplicating this backlog. |

### 6. Novel hypotheses and cheapest first experiments

These IDs preserve all original ideas. None is recorded as shipped from reusable
building blocks alone. Their novelty versus other tools remains unverified. Choose
pilots after the higher-priority instrumentation and real-workload evidence above.

| ID | Status | Idea / existing building blocks | First experiment and acceptance |
|---|---|---|---|
| N-01 | evaluation-needed | **Subtractive pass:** ddmin added hunks/lines/helpers after tests pass, reusing automatic engines, equivalence gate and Git recovery. | Try 50 historical owner patches; report removable lines, behavior/test/equivalence scope, execution cost and failed candidates. Engine mutations alone are not a patch minimizer. Feeds T-05. |
| N-02 | evaluation-needed | **Hallucinated-symbol gate:** resolve new identifiers, imports and arities before a build. Graph/syntax/import-declaration checks cover narrower facts. | Count transcript failures attributable to unresolved symbols, then prototype precise supported-language resolution; prove dynamic/uncertain names do not silently become hard false positives. |
| N-03 | evaluation-needed | **Negative-memory tombstones:** record failed/reverted/slower approaches against source hashes and expire on change. Reuse outcome ledger, memory and briefs. | Mine repeated failed approaches; test useful resurfacing, retirement and stale-match false positives. Existing outcome storage is not a tombstone workflow. |
| N-04 | evaluation-needed | **Executable memory:** store facts with invariant probes; label unverified facts and retire failed facts. Reuse `invariant_probe`, test contracts and memory store. | Convert 20 owner-note facts to bounded probes; report false/current/unverifiable facts, source bindings and probe cost without granting memory approval authority. |
| N-05 | evaluation-needed | **Comprehension cost:** signature + folded callees + touched types, joined to reread telemetry. | Rank symbols in ARIA by token cost/reread frequency and compare with owner judgments; test usefulness before making a refactor gate. |
| N-06 | evaluation-needed | **Prompt-cache-aware layout:** stable ordering/frozen prefixes for briefs, folds and rules. Reuse deterministic context output and usage ledger. | Compare observed provider cache-read tokens under stable/default ordering with matched inputs/tasks; report prefix invalidation and total cost. |
| N-07 | evaluation-needed | **Session passport:** portable task, hash-bound reads, decisions, tombstones and pending checks for cross-client handoff. Existing task/journal/recovery primitives are insufficient alone. | Hand off one half-finished task between two clients; compare reread tokens/outcomes and force rereads when hashes change. |
| N-08 | evaluation-needed | **Blast-radius edit gate:** warn/block edits when current callers were not inspected this session. Reuse graph, read receipts and hash-checked edit admission. | Measure regression association in R-INF tasks, then test stale/incomplete-graph and false-block behavior before enabling enforcement. |
| N-09 | evaluation-needed | **Per-model scorecards:** join findings, survivors, performance and reverts to model/client attribution. Reuse agent/outcome ledger and commit trailers. | Backfill a month of commits with explicit unknown/mixed attribution; compare task types and sample sizes before routing decisions. Existing mutation attribution is not a complete model scorecard. |
| N-10 | evaluation-needed | **Budget contracts:** per-task token/turn budget and targeted spend advice. `ledger/budget.rs` already tracks budgets; REP-02 supplies call data. | Run soft warnings and measure overrun/task outcomes, including overhead; evaluate suggestions such as repeated reads→fold without reinstating rejected advice unmeasured. |
| N-11 | deferred | **Speculative pre-folding:** graph/navigation predictions precompute future views and token counts. | First measure offline next-file prediction on transcripts; only prototype if expected benefit exceeds invalidation/preparation cost. |
| N-D1 | evaluation-needed | **Hash-bound docs:** sections declare described symbols; changed source flags possible staleness. | Annotate `docs/native-harness.md`, observe a week of changes and judge false alarms; source drift is a review trigger, not proof prose is wrong. |
| N-D2 | evaluation-needed | **Agent cards:** generated ≤300-token module purpose/entrypoint/invariant/gotcha/tombstone cards, hash-bound and requested via briefs. Existing knowledge cards are narrower primitives. | Compare large instruction preload with on-demand cards by tokens and verified outcomes; show stale-card handling. |
| N-D3 | evaluation-needed | **Docs from confusion:** cluster failed searches, API misreads and user questions from R-INF-04. | Produce top-ten confusion clusters from a month of owner sessions, with redaction and validated proposed docs/card changes. |
| N-D4 | evaluation-needed | **Executable docs everywhere:** extract Markdown shell/code blocks into confined checks with expected outputs/hashes. | Pilot README/install commands with explicit fixtures/resources and opt-ins; distinguish examples requiring credentials/destructive actions from executable verification. Existing local checks do not validate every Markdown block. |
| N-D5 | evaluation-needed | **Prose claim checker:** numerical claims cite receipts and superseded/unreferenced claims are flagged. Generalizes DOC-01. | Run on Forge docs with labeled numeric examples and receipt scope/freshness; catch the MCP measurement drift without treating every number as a performance claim. |
| N-D6 | evaluation-needed | **Instruction pruning:** measure rule exercise, violations and preload tokens before suggesting removals. | Observe per-rule usage for a month; report unobserved versus redundant rules separately and propose, rather than automatically delete, instructions. |

### 7. Carried-forward engineering boundaries without original IDs

These five entries cover engineering scope that cannot be honestly folded into a
research ID. They consolidate the prior roadmap/migration program rather than
create a second ordered list. Actual client adoption is already DOC-05/R-INF-01 and reporting is REP/T.
CF-04 retains prompt transformation and task-aware recommendations beyond catalog
descriptions; CF-05 retains broader inbound/outbound routing beyond HTTP, Tasks
and conformance. They reuse those entries' measurements without duplicating their acceptance.

| ID | Status | Carried-forward scope / current evidence | Remaining acceptance / done when |
|---|---|---|---|
| CF-01 | implemented | **Scoped mutation profile restoration**, tracked in [#196](https://github.com/mcpirate17/llm-forge/issues/196). Its requested practical Cargo dependency/workspace and external Fest/Mull fixtures are delivered by #203/#204/#214/#215, with real automatic strong/weak/failure controls, provenance rejection, cleanup and publication checks in the linked contracts. | Original fixture-based restoration is complete within declared profiles; reconcile/close the issue accordingly. Keep unsupported arbitrary builds, pins/grants and standalone Mull restrictions explicit. The actual Forge campaign timeout remains incomplete evidence, not a defect or automatic requirement to keep expanding scope. A future package/workload pilot needs a concrete user need and bounded plan. |
| CF-02 | open | **Conservative affected-test intelligence.** Native test-import reporting is advisory; it does not prove a selected test set catches all relevant regressions. | Define supported languages/flows and uncertainty, retain a safe fallback, and compare selected/full outcomes against independent controls before promoting test skipping. Reuse reports/graph and link T-01 evidence. |
| CF-03 | partial | **Remaining native caller and clean-host closure.** Shared native surfaces and default native install exist; live `Makefile`/`conductor.mk` still call Python for `slop-backlog`'s first gate, `slop-imports`, `slop-probe`, duplicate audit, mutation retention/reap, history/seed/worktree utilities, Codex journal, NotebookLM bundle, baseline metadata and initialization. [Migration status](../native-migration-evidence-status.md#remaining-migration) retains reachable Python, packaging, governance/report/workspace/graph/utility boundaries. Historical handoff lists are not fresh caller inventories. | Inventory live Make/CLI/API/hooks and dependencies, reuse native contracts, move every live caller before deleting Python, prove parity and clean install, then exact-tree checks/sweep. Keep external-host receipt acceptance, installation and actual-client activation separate. Retain the positive external-invariant collector/waiver case as needs-verification: live collector/probe code exists, but this review found no linked real automatic receipt with its manifest declaring the invariant; a successful leaf probe is insufficient. Hardware acceleration stays conditional on a measured embedded bottleneck, not an automatic GPU project. No blanket code-shrink, runtime or completed-migration claim. |
| CF-04 | evaluation-needed | **Native prompt strengthening and task-aware tool recommendations.** Reuse prompt contracts, deterministic composition, task modes, tokenizer counts, recovery and capability-filtered catalogs. Current literal retention does not establish preserved meaning/instruction hierarchy; rejected next-inspection advice is a retained control, not a delivered recommender. MCP-04/05 cover narrower discovery wording. | Compare explicit task-mode transformations/recommendations against unchanged prompts and a deterministic no-model baseline on matched verified tasks. Report intent/provenance retention, instruction priority, task success, tokens and runtime overhead. Any optional model stage is bounded, explicit and evaluated separately; recommendations grant no authority and never execute actions. Ship only supported modes after prospective evidence. |
| CF-05 | deferred | **Broader inbound/outbound workflow.** Current outbound support admits one operator-pinned stdio endpoint/tool through the shared confined owner; this is not multi-endpoint routing, authentication lifecycle or credential isolation across endpoints. MCP-07/09/10 address Tasks/HTTP/conformance, not this broader contract. | After real-client and transport/task-value controls, select only a demonstrated needed workflow. Prove per-endpoint routing/schema/executable authority, explicit network and credential grants, isolation and authentication/rotation/expiry lifecycle, bounded scheduling, cancellation/disconnect behavior and cleanup. Distinguish suppressed responses from daemon cancellation and undo; preserve the existing endpoint and no-credential defaults until separately authorized. |

The first implementation increment should be selected from sections 1–2 against
actual user work, rather than automatically extending mutation infrastructure.
Optional research pilots remain bounded and independently reviewable.

## Optional marketing, after evidence

Marketing is not a dependency of this program. Preserve the original options:
lead with the four user problems and scoped evidence; publish actual research
findings; lower trial cost with an inspect-only quickstart, prebuilt Linux binaries
or `cargo binstall`/devcontainer where maintained; consider registry metadata,
GitHub topics, a short demo, tagged releases and client plugin packaging; document
the threat model for security audiences. Verify current registry requirements,
licenses, install support and platform claims before publishing. No claim of unique
coverage, free-OSS competitor status or task-level savings is established here.
Potential channels remain a blog/dev.to, Show HN, r/LocalLLaMA, r/ClaudeAI, r/rust,
Lobsters and relevant security meetups; no publication or outreach is requested.

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
