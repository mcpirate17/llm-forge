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

### Ratchet iterations

`make mutation-engine-run` writes its receipt under
`campaigns/receipts/.iterations/` (gitignored): a ratchet loop that commits
every iteration was growing the tracked tree by ~350 KB per run, and every
clone and CI checkout paid for it. When a loop settles,
`make mutation-receipt-promote` copies the newest iteration receipt into the
tracked directory; only that copy is committed.
