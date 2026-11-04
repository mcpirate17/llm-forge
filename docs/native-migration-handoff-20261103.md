# Native upgrade handoff — 3 November 2026

The interrupted release batch is shipped. **The broader native migration is
unfinished.** Continue from the merged source below; do not recreate or recommit
the release batch. This handoff records remaining functions, integration work,
missing measurements and delivery boundaries. It is a point-in-time handoff,
not a replacement for the repository's GitHub issue tracker.

Snapshot checked at **2026-11-04 02:22 UTC / 2026-11-03 21:22 EST**.

## 1. Starting state and what is already finished

| Item | Verified state |
| --- | --- |
| Repository | `/home/tim/Projects/llm-forge` |
| Local and remote `main` | `84b8765896619b4bda45c18fcc1bd40dda75af52` |
| Committed tree | `77e8e1355b1ab2201eb7773cf5250130c1ea3757` |
| Delivery | [PR #183](https://github.com/mcpirate17/llm-forge/pull/183), merged |
| Working tree before this document | Clean; only local branch `main`; no pending source commits |
| Global executable | `/home/tim/.cargo/bin/forge`: `0.8.21 (git 84b8765)` |
| LLM host executable | `/home/tim/Projects/LLM/.venv/bin/forge`: `0.8.9 (git 2e667a9)`; host migration remains open |
| Forge checkout's old venv executable | `.venv/bin/forge`: `0.8.8 (git f04070c)`; use the explicit global executable for current commands |
| Existing nightly sweep | Enabled/active, daily 03:45 local; service now invokes the global `0.8.21` executable |
| Open GitHub issues at audit | None; recheck before starting implementation |

Already committed and merged:

- `0cb1100`: directory-capability reuse in console-script relocation, retaining
  identity/no-follow checks, publication, locking and durability behavior.
- `e450649`: completed README/architecture/value/audience documentation;
  `native/product-value/` measurement tools; seven historical result artifacts;
  missing [measurement report](native-product-value-measurements.md); stricter
  profiler provenance and failure handling; local-check/sweep coverage; compatible
  TLS dependency patches resolving six RustSec findings.
- `84b8765`: merge of the above. No old untracked release files need collecting.

Release evidence belongs to that exact committed source:

- Full local check and verification passed, with 18 steps. Local receipt:
  `.git/forge-checks/run-1793754812392715239-94404/receipt.json`;
  SHA-256 `f807c33f1292cc1077efd0bbdef348de5c16eddf687626a126b37c10d9f4fb44`.
- Fresh sweep: all 12 steps passed, GREEN, 1,851 seconds. Local immutable report:
  `research/reports/sweep/20261104T011354Z/summary.md`. Includes fresh-prefix
  installation, compilerless runtime and four lockfile audits. This is a defined
  persistent-clone/environment proof, not every operating system or host deployment.
- Installed global binary SHA-256:
  `920531541cbd76c1423771fd29ddca978363e3db1adb88b6414666eb5eecffec`.
- Audit found zero vulnerabilities; the existing `paste` unmaintained-package
  warning remains. Test matrices overlap; do not sum them into a unique-test total.

The receipt and sweep report are local generated evidence, not tracked release
files. Preserve their paths/hashes when handing work to another machine. A new
source commit needs its own checks; these receipts do not certify future changes.

## 2. Feature readiness against the original upgrade scope

| Area | Available now | Completion still needed |
| --- | --- | --- |
| A. Structured requests | Native schema validation, constrained-output support and explicit dispatch boundaries | Prove real caller adoption and declared repair/error behavior across the intended client workflows; schema/grammar support alone is not integration closure |
| B. Source understanding and editing | Multi-language syntax inspection, expected-hash edits/recovery, structural graph, BM25/exact/HNSW retrieval | Retire remaining interpreted callers; resolve external graph compatibility, seeding/publication and reuse orchestration; establish broader retrieval quality |
| C. Persistent execution | Native persistent PTYs, reconnect/write identities, task records, Linux enforcement and recovery mechanisms | Actual-client setup/lifecycle proof; finish maintenance/cleanup callers and their ownership/retention semantics |
| D. Context handling | Pinned BPE counting, bounded projections/folds, protected groups, compaction and recoverable originals | Measure complete agent tasks, provider usage, recovery/reread costs and harmful omissions |
| E. Hardware acceleration | Optional external model/embedding services; no demonstrated embedded tensor bottleneck requiring a new GPU backend | Conditional only: first identify and measure a qualifying embedded workload; do not start a CUDA/Triton project merely to tick a box |

See [architecture](../ARCHITECTURE.md), [structured output](structured-output.md),
[harness](native-harness.md) and [benefits](benefits.md). Native components and
passing tests establish implementation properties, not universal speedups or
better coding-agent outcomes.

There are **83 Python files under `src/`, including four testdata fixtures** at
this snapshot. That is an inventory count, not 83 independent ports or a completion
percentage. Classify live orchestration, compatibility APIs, guest integrations
and fixtures before deleting anything. Python used to run a Python target's tests
is different from Python required to orchestrate Forge itself.

## 3. Work to finish, in suggested dependency order

### P1 — Automatic mutation campaign generation and execution

**Current boundary:** `forge mutation plan` reports a plan; `results` parses
reports. Neither replaces the complete automatic writer/runner.

- Remaining functions: `src/conductor/mutation_campaign_generate.py::{plan,write}`;
  `mutation_engine_generated.py::{load_generated_campaign,run_generated_campaign,score}`;
  `mutation_engine_{fest,cargo,mull}.py::execute`. `Makefile:113–130` still calls Python.
- Reuse native scope/model/manifest and receipt primitives, including
  `native/conductor-native/src/mutation_manifest.rs` and
  `native/forge/src/mutation_plan.rs::run`; avoid duplicating their algorithms.
- Finish native manifest writing, engine execution, snapshot lifecycle, attribution,
  scoring and receipt publication; migrate the public Make/entrypoint callers.
- **Done when:** exact changed sources/tests are enforced; execution opt-in and
  disposable snapshots remain; automatic fest/cargo-mutants/Mull runs reproduce
  the contract; drift, baseline failures, timeouts, survivors, cancellations and
  incomplete runs remain non-PASS; cleanup and source/test identities are verified.
  Include negative cases and complete planned/completed/failed/censored/unstarted
  accounting. Never substitute hand-authored mutants or synthesized receipts.

### P2 — Mutation evidence, coverage and governance compatibility

- Remaining functions: `mutation_testing.py::{verify_evidence,receipt_drift}`;
  `mutation_coverage.py::{coverage_report,verify_changed,canary_report}`;
  mutation retention/registry/receipt maintenance helpers. Callers remain in
  `Makefile:147–156` and `conductor.mk`.
- Reuse `native/forge/src/mutation_receipt_verify.rs::run`, candidate review/waiver
  collectors and native manifest handling. Byte-binding verification does not
  replace registry coverage, retention policy or authentic execution evidence.
- Finish native inventory, changed-test checks, canary/coverage reports, retention
  and their caller migration. Preserve externally granted policy boundaries.
- Portable grandfather configuration is already implemented through operator-owned
  `MutationGrandfather` grants (`commit`, `tree`, `inventory_path`, `inventory_sha256`)
  in `candidate_execution_grants.rs` and `candidate_mutation_definitions.rs::grandfather`.
  Correct the stale claim that it is unfinished in `docs/native-candidate-review.md`;
  document strict default behavior and demonstrate portable host integration.
- Verify a positive external-invariant collector case using a real automatic
  campaign receipt whose manifest declares that invariant; a successful leaf
  probe is insufficient. Historical documentation still reports this evidence gap.
- **Done when:** native reports/verdicts match applicable contracts, bad or stale
  evidence fails explicitly, policy cannot self-authorize exemptions, and callers
  stop importing the replaced Python APIs. Passing leaf tests alone do not establish
  end-to-end host compatibility.

### P3 — Reuse, duplicate, equivalence and audit workflows

- Reuse orchestration: `src/conductor/reuse/sweep.py::{discover_scope,detector_commands,
  run_detector,native_inventory,main}` plus detector, consolidation, graph, file-family
  and repository-evidence modules. `run_duplicate_audit.py` remains reachable from
  `conductor.mk:194–218`.
- Native `reuse_consolidation.rs::decide` and `native_reuse.rs` provide algorithms;
  they do not establish a complete shipped native reuse-sweep workflow.
- Other live flows: `equivalence_gate`, `import_ablation`, `equivalence_probe`,
  `guardrail_audit`, duplicate/target configuration and `candidate_review/vulture_*`.
  Native `equivalence-worker` and `forge candidate` are useful building blocks.
- **Done when:** native commands preserve scope, baseline/waiver decisions, bounded
  detector execution, explicit unavailable/failure states, provenance and output
  contracts; compare representative real inventories; switch Make/API callers and
  only then remove the retired implementations. Keep target guest runtimes explicit.

### P4 — Workspace retirement, graph compatibility and remaining utilities

- `worktree_reap.py::{inventory,active_process_cwds,blockers,decide,_recheck,apply}`
  remains a separate workflow. Native `forge worktree-runtime` relocates console
  launchers; it is not a worktree reaper. `workspace-hygiene` reporting is not
  equivalent to authorized deletion.
- Preserve live-process checks, claims/leases, remote/dirty state, archive and
  checkpoint retention, unknown-state refusal and final rechecks before retirement.
  `crg_seed_worktree.py` and host graph-probe callers also need migration.
- Native `.forge/graph.db` and external `.code-review-graph/graph.db` have different
  schemas/semantics. Finish native graph seeding and shared publication/freshness
  work, then migrate consumers explicitly; do not silently redirect schema-9 clients.
- Inventory remaining `conductor.mk` utilities: `ci_history_fetch`, `dead_tests`,
  `codex_journal`, `notebooklm_bundle` and their host/API consumers. Map each to an
  existing native function or implement a bounded replacement before retirement.
- **Done when:** each public caller has demonstrated output/failure/ownership parity,
  graph identity/freshness is checked, and no dependent API is removed prematurely.

### P5 — Distribution and LLM host adoption

**This is still open despite the global binary upgrade.** Root `pyproject.toml`
ships a Python conductor wheel, three mutation entrypoints and a pytest plugin.
The native default executable no longer discovers Python, but that does not
retire all shipped orchestration or host hooks.

1. Inventory imports, shell entrypoints, Make recipes, provider hooks and optional
   bindings. `candidate_review/identity.py` and `ownership.py` still support campaign
   generation; the pytest plugin may remain a justified guest integration.
2. Finish caller parity before removing packages/shims or runtime dependencies.
   Anything shipped imports or shells out to must remain declared in package metadata.
3. Update the LLM dependency pin/lock and client configuration as a **separate LLM
   change**, under that repository's live ownership and governance contract.
4. Test clean installation, actual client discovery/calls, startup/A2A/project jobs,
   permitted/denied/error hook paths and rollback. Verify which executable each
   client actually uses; package installation does not restart existing processes.

Concrete host findings, recheck before modifying:

- LLM pins `conductor-tooling` to `2e667a944c0b434ed278340ec1bb4ec49d6003b1`
  in `pyproject.toml:264`. Its Forge binary has inode `56098955`, link count **4**;
  it is shared with research environments. Never truncate/copy over it in place.
- Active research was observed in `~/.cache/llm-forge/worktrees/pi-qwen-fab-rsi`;
  existing CRG and embedding services also remain. Recheck owners/processes before
  any environment replacement or service restart.
- Claude has five malformed commands: four concatenate `FORGE_MODE=warn` with the
  absolute executable path; SubagentStop concatenates it with
  `FORGE_HOOK_STANDALONE=1`. Repair exact commands without duplicating hooks.
- Codex, Qwen and Grok each retain four legacy dispatcher hooks and a Python A2A
  startup call. LLM's Makefile also calls removed `tooling.hooks.dispatch.native_freshness`.
- Native Claude/Codex installers cover guards/lifecycle. Qwen/Grok installation
  covers bounded A2A startup only: complete the missing native guard/project-job
  behavior or explicitly retain required compatibility until it is replaced.
- Host provider directories are `0775`; observed configuration files are `0664`.
  Native provider installation refuses group-writable parents/files. Inspect current
  ownership and prepare exact required paths; do not recursively chmod the host.
- `--replace-legacy` recognizes defined command forms. It may retain malformed or
  custom commands. Review its dry-run plan and preserve unrelated configuration.

**Done when:** version-coherent installation and lock metadata match; real host
callers use the intended native paths; required project hooks survive; no shared
research environment is damaged; retained guest interpreters are documented.
See [provider contract](native-providers.md).

## 4. Work to start or supply missing evidence for

These are evidence/product gaps, not reasons to rerun all completed migration tests.

| Work | Required deliverable |
| --- | --- |
| Persistent MCP versus direct CLI | Same tools/results; cold service/adapter setup separated from warm calls; include declared catalog, policy and transport work; wall/CPU/RSS and complete outcomes |
| Hooks enabled versus disabled | Matched events; event p50/p95, actual injected/provider tokens, denied/failed events and task outcomes |
| Agent tasks with versus without Forge | Same model, budget, repository and tasks; repeated trials; verified patches, actual usage, wall time and failures/censoring |
| Post-optimization Python/native relocation | Fresh source/binary-bound paired run; preserve safety/durability scope, COW, bytes/modes and idempotence checks; retain the original slowdown results |
| Broader retrieval evaluation | Human-adjudicated relevance and real coding-task outcomes; include indexing/embedding cost and freshness failures |
| Practical onboarding | Tiny copyable example, read-only MCP profile, readiness/doctor flow, client-specific setup/status/undo and actual-client validation |
| Discovery and audience validation | Observe real tool selection and onboarding failures; validate audience hypotheses with users before making adoption/superiority claims |

Existing relocation evidence found native wall time **1.38× / 10.72×** Python
at 16/256 scripts (`ef50dcb`). The subsequent native/native optimization reduced
256-script CPU median **29.74%**, with only **1.63%** lower wall median and no
stable wall-speedup claim. Existing projection counts omit content and are not
billed-token savings or task-quality proof. Preserve these limits in new reports.
See [benefits](benefits.md), [measurements](native-product-value-measurements.md)
and [product research](product-research.md). Plan bounded experiments first;
this handoff does not launch continuous improvement or multi-hour model runs.

## 5. What needs to be committed

| Change | Commit state / boundary |
| --- | --- |
| Interrupted Astra release and completion fixes | Already committed and merged in PR #183; nothing to recover or recommit |
| This handoff | New, uncommitted `docs/native-migration-handoff-20261103.md`; documentation-only commit when requested |
| P1–P4 implementation | Future cohesive commits containing native behavior, migrated callers, applicable tests, dependency declarations and contract documentation; remove Python only with demonstrated closure |
| P5 Forge packaging/client support | Forge commits/PRs after compatibility tests; do not bundle unrelated host research edits |
| P5 LLM pin, lock and hooks | Separate LLM repository change after live ownership checks and caller parity; retain required governance/mutation evidence there |
| New measurements/onboarding | Separate source-bound tools/results/docs commits; preserve historical benchmark JSONs and record new trials as new artifacts |

Do not commit build outputs, venvs, caches, `.git/forge-checks`, ignored sweep logs,
local systemd units or runtime graph databases. These can be evidence/deployment
artifacts without being source changes. No further source files were modified
while preparing this handoff.

For future delivery: follow `AGENTS.md`, use explicit paths, a temporary branch
and PR, Conventional Commit subject plus `Agent:` trailer. After committing,
run `make local-check` and `make local-verify` on the exact head/base before PR
publication; rerun if either changes. There is no hosted CI. Forge does not require
LLM's mutation policy for ordinary landing; automatic mutation safeguards still
apply whenever campaigns are run. Do not fabricate grants for native governance
review commands that require external operator authorization.

## 6. Restart checklist

1. Recheck `git status`, local/remote head, open issues, live processes and claims.
   Read the current repository contract and structured active state; preserve all
   unrelated work. This snapshot grants no ownership of a future shared checkout.
2. Start with one bounded package, preferably **P1**; define native CLI and failure
   contracts, then inventory exact callers before editing. Reuse existing native
   algorithms rather than producing a second implementation.
3. Use `/home/tim/.cargo/bin/forge` explicitly until old venv launchers are retired.
   Work in the existing Forge checkout; its contract prohibits scratch worktrees.
4. Refresh graph evidence before relying on it. At this audit the external CRG
   was built at `d989847`, not `84b8765`; minimal context refused stale data and
   requested `openai/qwen3-embed-cpu` semantic search was disabled. Source/caller
   inspection supplied this inventory; stale zero results are not absence evidence.
5. Validate the package, migrate its callers, document remaining boundaries and
   deliver through the required PR flow. Report implementation, installed version,
   live activation and measured benefit as separate results.
