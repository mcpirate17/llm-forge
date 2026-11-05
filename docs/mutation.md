# Mutation testing in llm-forge

How campaigns are generated, run and audited. `forge mutation plan|write` is the
native campaign writer and `forge mutation inspect|run` the native runner; hosts that
have not migrated keep `conductor.mutation_campaign_generate` and
`conductor.mutation_engine_generated`. The remaining commands live in
`conductor.mutation_patch_audit` (corpus audit), `conductor.complete_scope_drift`
(local check for `complete`-mode scope drift before the audit catches it) and
`conductor.mutation_retention` (sweep); `make mutation-*` wraps the common flows.
Forge ships these capabilities without requiring mutation campaigns or receipts for
its own changes. A host such as LLM can adopt a separate evidence policy.

## Optional risk-ranked planning

`forge mutation plan` reports what `forge mutation write` would publish. Without
`--only` it covers the files this branch changed against `--base` (default
`origin/main`), plus dirty files inside this lane's active ownership claims; a dirty
checkout with no claim is refused, as is a branch with nothing changed. `--all-files`
is the explicit whole-tree maintenance sweep. `--verbose` adds the full manifests.
Refusals print `{"status": "REFUSED", "error": ...}` and exit 4. `write` refuses to
replace an existing manifest, whose recorded survivor baseline would be lost, unless
`--force` is given.

The planner can also rank an exact changed-file scope without running an engine or
writing campaigns:

```bash
forge mutation plan rust \
  --priority-policy candidate_policy.toml --priority-base origin/main \
  --only native/forge/src/mutation_write.rs
```

`--priority-policy` must name the host's configured candidate policy, resolved
through `[tool.conductor]` and the existing environment override. Only its
explicit `[risk].high` production/validation paths are eligible. A different
permissive policy cannot widen the host's scope. Missing policy data fails
explicitly; discovery paths remain exempt. Priority requires exact `--only`
paths and a verifiable base commit; it does not support a whole-tree inventory.
`--priority-base` defaults to `--base`, and the Python
`mutation_campaign_generate plan/write` wrapper exposes the same `--priority-policy`
option. Commands without the priority option retain ordinary planner behavior.

Rust source ownership follows declared targets and module reachability when a
package borrows source from another package's directory. CLI-only modules in
Forge's external `forge-cli` target are currently refused explicitly; assigning
them to the neighboring `forge` library would test the wrong package. Ordinary
library scopes remain available. The #184 reproduction is addressed by moving the
planner and lane identity into the shared library:
`forge mutation plan rust --only native/forge/src/mutation_plan.rs --include-covered`
now selects package `forge`. `cargo mutants --list --manifest-path
native/forge/Cargo.toml --package forge --file src/mutation_plan.rs` discovers
38 mutations at this change. Discovery is not execution or mutation PASS evidence.
Other CLI-only modules still require a supported package/source layout.
Without `--include-covered`, an already registered crate is skipped; `READY` with
zero newly generated campaigns is not evidence that its current sources passed.
This is bounded syntactic ownership discovery, not Cargo macro expansion. Unknown
item-generating macros and conditional path transformations are refused rather
than treated as proof of ownership.
Explicit imports that shadow `include!` are refused; glob-imported macro names,
procedural expansion and function-generated modules are outside this syntactic
analysis. Engine discovery and execution remain separate evidence.

Priority produces one campaign per eligible changed source. Cargo `--file`
patterns remain package-relative; source/test hash keys remain repository-relative.
Parsed Cargo target/module reachability excludes proven test-only files, defers
unknown roles, and preserves production files containing inline tests. A changed
test cannot pull an unchanged production file into the authorized scope. Missing
in-scope test/source pairings are visible diagnostics, without inventing a new
mutation obligation for exempt discovery code.

The deterministic order is source novelty, changed paired tests, a matching
caller-supplied impact hint, then source path. New source precedes changed source;
a byte-identical rename gets no novelty bonus. This is an ordinal heuristic,
not a calibrated failure probability. Unknown test identities are reported as
gaps, not counted as demonstrated new tests. Existing manifests never establish
current mutation PASS or coverage: those fields remain unknown in this plan.

`--priority-telemetry PATH` accepts optional **unverified caller-supplied hints**
under `campaigns[CAMPAIGN_ID]`. A hint row must match the emitted `tree_oid`,
`worktree_sha256`, `policy_sha256`, `source_sha256` and `test_sha256` fields.
Optional numeric fields are `cost_hint_seconds` and `dependency_impact_hint`.
Matching public hashes do not authenticate the producer or establish observed
runtime, current graph reachability, binary dependencies, or ignored artifacts.
Missing or mismatched hints remain unknown.

`--priority-budget-seconds N` uses matching supplied cost hints to select campaigns.
Unknown costs and campaigns exceeding the remaining estimated budget are visibly
deferred. This budget is an estimate, not an engine timeout or execution cap.
Every deferred row remains in `priority.ranked`; no engine is launched implicitly.

### Native test admission

When a host enables its mutation-evidence admission check, native-only changes
participate alongside Python tests. Rust test identities come from parsed test
definitions and actual Cargo/module context, including external test modules;
source hashes include those external test files. Attribution must also bind the
Cargo test target. A matching leaf function name or another executable's result
cannot prove a test ran or killed a mutant.

Recognized test-generating macros, ambiguous module/target contexts and unavailable execution
mappings fail explicitly as unsupported evidence. Ordinary production macros
are not compiler-expanded by this parser; accepting an ordinary production macro
does not prove that it cannot generate tests. C/C++ function spelling alone cannot
establish a CTest case: without a verified CTest-to-source mapping, admission remains
unsupported. These checks neither enable a host gate nor launch a campaign.

The current Cargo generator still emits a generic multi-target command and does
not bind `ranked_tests` or `value_analysis`. Consequently its generated campaigns
cannot yet satisfy per-test Rust value admission through this workflow. The bound
collector is usable only when an existing governed campaign supplies a verified
single-target command and ranked tests; raw or multi-target output remains
incomplete. Detecting native admission debt is separate from producing that
evidence, and no campaign or receipt is authored manually to bridge this gap.

## Receipt format

A mutation receipt is one JSON file with two blocks (slice L):

- **Summary** — plain JSON at the top level, byte-identical to what the
  pre-slice-L receipts carried: `campaign_id`, `status`, `generated_at`,
  score and killed/survived/timed-out counts, `source_sha256`,
  `runner_components_sha256`, engine, base commit. Every reader that only
  wants to know *how a campaign went* keys off these and never decodes
  anything.
- **Detail** — the per-mutant rows (`mutants`) and the value analysis
  (`test_value`) folded under one `detail` key, in one of three shapes:

  | `detail.encoding` | When | Shape |
  |---|---|---|
  | `json` | fewer than 50 mutants and under 32 KiB serialized | the detail keys inline |
  | `zstd+base64` | everything else | one `blob`, zstd-compressed base64 of the detail object's JSON |
  | `superseded` | an older receipt of a campaign that has a newer one | `{"superseded_by": "<newer file>"}` |

One decoder owns the format: `conductor.mutation_receipt_slim.expand_receipt`
(behind it, `receipt_expand_detail_native` in conductor-native). Readers that
need detail rows call `expand_receipt_field(receipt, "test_value")` and
decompress only when the field actually lives under the block; a summary-only
reader pays nothing. `forge receipt show <path>` prints a receipt expanded for
humans. Legacy receipts (no `detail` key) pass through every seam unchanged.

The engine writes slim receipts at every disk copy (the RUNNING stub, ERROR
receipts, and the final write); the in-memory dict stays full, so attribution
and the CLI summary are unaffected. Files are written in the canonical
receipt shape (`json.dumps(..., indent=2, sort_keys=True) + "\n"`, atomic
replace) regardless of encoding.

### Compaction

`conductor mutation_receipt_compact <dir>` is one pass over a receipt
directory: every kept receipt is slimmed, and every other receipt of a
campaign has its detail replaced by a `superseded_by` pointer. Which receipt
is kept is the audit's decision, not the clock's — the CLI runs
`mutation_patch_audit`'s own acceptance predicate first (a newer receipt can
be rejected because its runner components match neither this runner nor any
lineage entry) and hands the per-campaign keep-set to the native pass, so the
receipt the audit reads afterwards is exactly the one whose detail survived.
`--no-audit-keep-set` falls back to newest-passing-status, right only when no
receipt was lineage-rejected.

### Slim receipts and the evidence gate

The native evidence gate (`verify-evidence` behind `verify_mutation_evidence_native`
and `validate_mutation_receipt_native`) validates a receipt through
`receipt_errors`, which expands a slim receipt before any rule reads it — so the
`mutants` and `test_value` lists folded under `detail` are restored for the
ratchet cross-checks, a plain pre-slim receipt (no `detail` key) validates
unchanged, a `superseded` pointer is rejected with `receipt superseded by
<newer file>` and can never rank as evidence no matter what its summary says,
and a blob that does not decode is rejected with its own decode error rather
than judged on the summary alone. Between slice L landing the format and this
seam being added, every tracked receipt was slim and the gate rejected them all
with `mutants must be a non-empty list` — a real engine PASS unreadable as
evidence; no CI job ran `verify-evidence` at the time, which is why the gap
went unnoticed for a week (the job below exists because of it).

Every rejection is classified where it is produced, never parsed back out of
its message: each `receipt_rejections` entry is `{receipt, kind, detail}` with
`kind` one of `no_campaign`, `scope_error`, `not_pass` (any status other than
PASS, `RATCHET_HELD` included — one verdict, not a wall of follow-on schema
noise — plus the two stale-PASS bindings, source drift and a manifest revision
the campaign has moved past: the run was real, it is just not current, and the
campaign re-runs), `superseded`, `runner_map_mismatch` (the component map and the
core/adapter/scope-guard era bindings: same debt, the campaign re-runs),
`decode_error`, `schema_error` (the validator refuses the receipt's claim —
including survivors outside the baseline, which contradict the PASS it
asserts), `manifest_load_error`. The old free text survives verbatim as
`detail`, and the result carries a top-level `rejection_counts` aggregate
(schema `llm.mutation-testing.evidence-check.v2`; a receipt the loader cannot
parse at all counts as `decode_error`).

### CI

Forge CI checks that the shipped receipt tools work. It does not require changed
tests to have mutation evidence or require PRs to record missing-evidence debt.

The repo-wide canary, `uv run python -m conductor.mutation_coverage
canary` (locally `make mutation-canary`) re-checks every receipt in the tree
and exits 5 when any of them is unreadable, whoever's tests it covers, and 0
otherwise — however much evidence is missing. That is the check that would
have caught the #41–#46 gap on day one.

The retention report, `make mutation-retention`, runs in dry-run mode in CI. It
prints what a sweep would keep, delete, and leave to protection, and exits:

- **0** — the sweep ran; the plan on stdout *is* the report, a non-empty
  `deleted` count included. Receipts are supposed to accumulate between
  sweeps, so an uncitable list is a plan, not a failure;
- **2** — the sweep could not decide (an unreadable or unloadable manifest,
  a gate that would not run) and touched nothing; the reason goes to stderr
  and the job fails;
- anything else is an unhandled defect — a crash like the TypeError that
  kept `make mutation-retention` dead for a week fails the step it always
  should have.

`MUTATION_RETENTION_APPLY=1 make mutation-retention` performs the deletion
locally; `MUTATION_RETENTION_ARGS="--protect FILENAME"` shelters a receipt a
lane has in flight.

### Optional host evidence checks

A host that adopts a changed-test evidence policy can run
`uv run python -m conductor.mutation_coverage changed --base "$merge_base" --github`.
It inventories changed tests (merge-base diff, ACMR), checks their receipts, emits
annotations and a step-summary table, and exits:

- **0** — every changed test file has evidence;
- **6** — evidence is missing (`no_campaign`, `not_pass`, `superseded`,
  `scope_error`, or `runner_map_mismatch`); this is not a PASS receipt;
- **5** — a validator-side rejection (`decode_error`, `schema_error`, or
  `manifest_load_error`);
- **4** — REFUSED because of registry or campaign errors.

The host decides how these results affect its workflow. Locally,
`make mutation-evidence MUTATION_BASE=<ref>` runs this optional check against any
ref (`origin/main` by default). It is not a Forge landing requirement.

### The native runner

`forge mutation run <manifest> --allow-mutations` runs a generated campaign in a
disposable snapshot, then proves the pinned sources and tests held in both the
snapshot and the live repository before it scores anything.

| exit | meaning |
|---|---|
| 0 | `PASS` or `RATCHET_HELD` |
| 1 | a verdict that is not green: `FAIL`, `ERROR`, `INCOMPLETE` or `CANCELLED` |
| 4 | refused, with `{"status": "REFUSED", "error": ...}` on stdout |

`forge mutation inspect` exits 0 for `READY`, 3 for `NOT_READY` and 4 when it cannot
read the manifest.

Every receipt carries `accounting`, with `planned`, `completed`, `failed`, `censored`
and `unstarted` counts, and `complete_campaign`. `planned` comes from the engine's own
total, so a run that stopped early shows the gap. An unfinished engine, any
`TIMED_OUT`, `NO_COVERAGE` or `UNVIABLE` row, or an unstarted mutant prevents a
complete campaign and yields `INCOMPLETE` unless a new survivor or row error
already requires `FAIL` or `ERROR`. Known survivors cannot hide these gaps as
`RATCHET_HELD`. Cargo-mutants exit 2 is a completed survivor verdict only with a
reported survivor; other engine failures remain incomplete. `CANCELLED` means a
signal stopped it; `unstarted` stays null if the engine
had not yet said how many it planned. Neither is PASS evidence. A cancelled run never
records a survivor baseline, and neither does a run with unstarted mutants. An
`INCOMPLETE` verdict that reaches scoring with every planned mutant reported can
record one. That cannot weaken the ratchet: a censored mutant is not in the
baseline, so if it survives later it counts as a new survivor and the run is `FAIL`.

All three native engines require at least one scored (`KILLED` or `SURVIVED`)
mutant for **each selected source**. A versioned `source_coverage` report records
per-source counts and missing files. One measured source cannot conceal another
source with zero mutants: the run is refused (exit 4), its receipt is `INCOMPLETE`
with `complete_campaign: false`, and no survivor baseline is recorded. An engine
exclusion cannot waive a selected source. Supporting hashes outside the selected
source set are refused as ambiguous scope; the runner never expands mutation
authority to cover those files. Distinct symlink aliases are unsupported; redundant
dot segments are normalized.

Native validators independently recompute this report from source pins and mutant
rows. The scorer also checks the campaign's exact source pins. Missing or forged
counts cannot establish completion. These checks prove a nonempty measured mutant
set per selected file, not per-line coverage or exhaustive mutation operators.

The Cargo adapter spells a root-package manifest as `./Cargo.toml` when invoking
cargo-mutants, preserving a nonempty parent directory without modifying the
generated manifest. Nested and absolute manifest argument spelling is preserved.

Three things still need Python. fest is a Python tool, the campaign's interpreter must
be able to import `conductor` for the `mutation_pycache_evict` pytest plugin, and it
needs `pytest-cov` (the `mutation` extra), or the baseline fails on `--cov`.
`[tool.conductor].snapshot_python` names that interpreter. The current runner also
resolves an interpreter for Rust and Mull runs to support guest test adapters;
removing that unconditional dependency remains migration work.

A receipt names its runner with `runner_kind: forge-native` and a `native_runner`
block in place of the Python runner's hashes. A build whose native fingerprint
inputs differ from HEAD records `source_matches_git: false`, and validators reject
its receipts. Unrelated dirty documentation does not change that stamp. Evidence
requires committed native inputs.

Native-runner schema v2 compares the compiled source fingerprint with the audited
native source inventory: source trees, Cargo manifests/lockfiles, build scripts
and declared embedded assets. The former v1 partial component list is rejected;
lineage cannot waive a stale native fingerprint. Missing Forge sources in a host
cannot establish current native-runner identity. This is source freshness,
not authenticated producer identity, compiler equivalence or binary attestation.
Captured candidate/corpus inputs retain their own freshness checks; ambient
compatibility reads remain filesystem observations.

Native process capture retains at most 16 MiB of stdout and 1 MiB of stderr.
It drains excess bytes so children can exit, then refuses completion; overflow,
read errors and reader failures cannot turn truncated output into a successful
run. Existing engine deadlines still bound execution.

### Ratchet iterations

`make mutation-engine-run` resolves the receipt root through
`forge project --host ROOT path mutation-receipt-root` and writes beneath its
`.iterations/` directory (`campaigns/receipts/.iterations/` in this repository).
`MUTATION_RECEIPT` and `MUTATION_RECEIPT_ITERATIONS` remain explicit overrides;
configuration failures stop the target before execution. The Make runner leaves
directory creation to the native runner after path confinement. A ratchet loop that commits
every iteration was growing the tracked tree by ~350 KB per run, and every
clone and CI checkout paid for it. When a loop settles,
`make mutation-receipt-promote` copies the newest iteration receipt into the
tracked directory; only that copy is committed. Promotion remains an
operator-controlled shell maintenance adapter using `mkdir`/`cp`, without the
native publication confinement or no-overwrite guarantee; its migration is pending.

Campaign identifiers are single filename components, at most 200 UTF-8 bytes,
starting with a letter or digit and containing letters, digits, dots, underscores
or hyphens. Native campaign and receipt publication reject parent traversal and
resolved paths outside the repository before creating output directories. Temporary files use exclusive
creation; unforced campaign publication cannot overwrite a concurrent winner.
Configured ancestors remain trusted against concurrent replacement.

### Bytecode isolation

Every child command an engine spawns — the baseline suite, the engine binary
(and the per-mutant test commands it spawns in turn), attribution's re-applied
mutants — and the gate's two pytest collect probes run against a
`PYTHONPYCACHEPREFIX` scratch that is private to the run, wired once in the
runner (the choke point all engine children flow through) and
`gate.preflight_pytest_config`. CPython validates a
`__pycache__` entry by source mtime and size, so a same-size rewrite within
one mtime second leaves a stale `.pyc` that later interpreters keep
executing — and applying a mutant *is* a same-size same-second rewrite, which
let a campaign grade the unmutated bytecode and report a survivor no source
diff explained (PR #49's spurious `arithmetic_op`). With a prefix set,
CPython stops reading beside-source caches entirely, so a stale one cannot be
read even where it already exists.

Only the mutated file can be stale — every other module is byte-identical
across the mutants of one run — so PR #53 caches instead of refusing to write:
`PYTHONDONTWRITEBYTECODE` is gone (PR #51's version set it, making every
child recompile every module and costing the retention campaign 2.6x wall
time), unmutated modules compile once into the run's prefix and are served
from it after, and each mutated source's cached `.pyc` — plain, `.opt-1` and
`.opt-2`, the paths `importlib.util.cache_from_source` maps — is evicted
before every child launch that must not trust it (`isolated_python_env`).
fest's subprocess backend builds each mutant's pytest command itself, so no
launcher sits between the rewrite and the import; the adapter loads a pytest
plugin (`conductor.mutation_pycache_evict`, via `PYTEST_ADDOPTS`) that repeats
the eviction inside every child before collection imports anything, and
attribution evicts around every re-applied mutant. Measured on
`conductor-67_mutation_retention_fest_20260913` (`MUTATION_BASE=ff91c95`,
outcomes and survivor set identical in every run):

| | wall clock (median of 3) | KILLED/SURVIVED/NO_COVERAGE |
|---|---|---|
| pre-#51, no isolation (single run) | 51.1 s | 90/8/19 |
| #51, refuse to write (single run) | 131.8 s | 90/8/19 |
| #51 as merged (3 runs) | 127.67 s | 90/8/19 |
| #53, cache + evict (3 runs) | **51.12 s** | 90/8/19 |

### Orphaned runs

The runner starts every engine in its own session and binds it to the
engine process's lifetime with `PR_SET_PDEATHSIG`, so the kernel kills the
engine binary the moment the process that spawned it dies (Ctrl-C, session
end, OOM, sandbox teardown) — but the mutants that engine already spawned
inherit nothing and may keep running. The native runner serializes registration
and removal under a persistent interprocess lock. Its separate
`native_pgids.json` uses a `native` array and preserves diagnostics when process,
pipe or capture cleanup fails. A recorded numeric PID/PGID is not authority to
signal a later process: manual recovery must establish current process identity.

Legacy Python runners use `live_pgids.json` with a `live` array. The legacy
`make mutation-reap` command does not consume native records, including with an
older installed Python adapter. Native supervision handles ordinary completion,
timeouts and cancellation; verified automatic recovery after a hard-killed
native supervisor remains a separate capability to implement.


## Contextual new-test admission

Ordinary `verify-evidence` keeps its canonical receipt projection. A host may
add exact node/source requirements under `[tool.conductor.mutation_value_contexts]`:

```toml
[tool.conductor.mutation_value_contexts]
"tests/test_flow.py::test_flow" = ["pkg/training.py", "pkg/selection.py"]
```

These requirements are additional obligations, not waivers. Each required source
needs a current registered automatic PASS original, independently checked by the
unchanged native scope, manifest, source/test hash, runner and provenance rules.
The test must have CORE or INTENTIONAL_REDUNDANCY value and actually kill a
reported mutant in that source. Every reported kill must be attributed; baseline
failures, survivors, errors, timeouts and missing value evidence block admission.
Unrelated positive source contexts cannot satisfy the declaration.

The latest attempt is selected separately within each exact final context. A
failed or partial latest attempt prevents fallback to an older PASS in the same
context, and contradictory classifications fail closed. A newer MERGE receipt
in another source context cannot erase a valid required witness. MERGE rows
remain in the original evidence. Candidate review retains the original receipt
paths, byte hashes, bindings and value rows in
`contextual_mutation_witnesses.json` in its runtime artifacts. It never writes a
combined receipt or changes a classification. Hosts without declarations retain
canonical value admission and now also reject incomplete nominal PASS receipts.
