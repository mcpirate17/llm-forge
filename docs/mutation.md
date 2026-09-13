# Mutation testing in llm-forge

How campaigns are generated, run and audited. The commands live in
`conductor.mutation_campaign_generate` (write/refresh), `conductor.mutation_engine_generated`
(run), `conductor.mutation_patch_audit` (corpus audit) and `conductor.mutation_retention`
(sweep); `make mutation-*` wraps the common flows.

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
noise), `superseded`, `runner_map_mismatch` (the component map and the
core/adapter/scope-guard era bindings: same debt, the campaign re-runs),
`decode_error`, `schema_error` (the validator refuses the receipt's claim —
including survivors outside the baseline, which contradict the PASS it
asserts), `manifest_load_error`. The old free text survives verbatim as
`detail`, and the result carries a top-level `rejection_counts` aggregate
(schema `llm.mutation-testing.evidence-check.v2`; a receipt the loader cannot
parse at all counts as `decode_error`).

### CI

The `mutation-evidence` job asks two questions of every PR. First, the tests
the PR changed: `uv run python -m conductor.mutation_coverage changed --base
"$merge_base" --github` inventories them (merge-base diff, ACMR), checks each
against current receipts, emits `::warning` per missing path, `::error` per
validator-side rejection and a step-summary table, and exits:

- **0** — every changed test file has evidence;
- **6** — evidence is missing but every rejection is debt (`no_campaign`,
  `not_pass`, `superseded`, `scope_error`, `runner_map_mismatch`): the job
  prints `mutation evidence missing for N changed test file(s): debt, record
  it in the PR body` and passes. Exit 6 is an acknowledgement, not a pass —
  the debt goes in the PR body, per AGENTS.md;
- **5** — any rejection is validator-side (`decode_error`, `schema_error`,
  `manifest_load_error`): a receipt the validator cannot read or refuses, and
  the job fails;
- **4** — REFUSED (registry/campaign errors), the job fails.

Second, the repo-wide canary: `uv run python -m conductor.mutation_coverage
canary` (locally `make mutation-canary`) re-checks every receipt in the tree
and exits 5 when any of them is unreadable, whoever's tests it covers, and 0
otherwise — however much evidence is missing. That is the check that would
have caught the #41–#46 gap on day one. Locally, `make mutation-evidence
MUTATION_BASE=<ref>` drives the changed-test check against any ref
(`origin/main` by default).

### Ratchet iterations

`make mutation-engine-run` writes its receipt under
`campaigns/receipts/.iterations/` (gitignored): a ratchet loop that commits
every iteration was growing the tracked tree by ~350 KB per run, and every
clone and CI checkout paid for it. When a loop settles,
`make mutation-receipt-promote` copies the newest iteration receipt into the
tracked directory; only that copy is committed.

### Orphaned runs

`run_command` starts every engine in its own session and binds it to the
engine process's lifetime with `PR_SET_PDEATHSIG`, so the kernel kills the
engine binary the moment the process that spawned it dies (Ctrl-C, session
end, OOM, sandbox teardown) — but the mutants that engine already spawned
inherit nothing and keep running. Belt and braces: every live run also
records its pgid in `campaigns/receipts/.iterations/live_pgids.json`
(appended on start, removed on exit). `make mutation-reap` lists the
recorded groups whose engine is dead while the group still lives, and
`MUTATION_REAP_APPLY=1 make mutation-reap` SIGKILLs exactly those — nothing
outside the registry is ever signalled.
