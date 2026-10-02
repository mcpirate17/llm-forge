# Context efficiency

Forge shares context projection and hook decision algebra between its native CLI
and the Python dispatcher. It makes no model calls to compose context. Permission
votes, errors, approvals, blockers, and required instructions survive the budget.

## Bounded hook context

The aggregate hook context budget is `HOOK_CONTEXT_MAX_BYTES` (default 16000,
minimum 512). Context deduplicates byte-identical fragments, preserving meaningful
whitespace and case, places stable instructions first, and orders other fragments by
priority. A duplicate optional fragment cannot hide a required copy. Whole
optional fragments are omitted rather than cutting sentences in half. Their full
JSON records are written atomically under `CONTEXT_FRAGMENT_DIR`, or
`$LEDGER_ROOT/context_fragments` (default `/mnt/data/llm/ledger/context_fragments`).
The injected marker names an ordered recovery manifest listing every omitted
fragment's content hash, so multiple omissions can be recovered together.

An artifact write failure retains the original context and reports the failure.
Required context may exceed the configured budget; the response explicitly says
so. The budget does not weaken a permission decision or stop response. Existing
hooks can keep emitting `additionalContext`. Hooks with distinct sections can
instead emit `hookSpecificOutput.contextFragments`:

```json
[
  {"id":"policy","version":"v1","category":"instructions","content":"Stable project instructions."},
  {"id":"state","category":"state","priority":10,"content":"Current changed paths and next step."}
]
```

Each fragment has optional `id`, `version`, `category`, `priority`, and
`protected` fields and a required string `content`. Categories `instructions`,
`policy`, `blocker`, `approval`, and `error` are required even without an explicit
`protected` flag. The merged hook response emits ordinary `additionalContext`;
fragment metadata does not leak into the harness response schema.

Compose a saved fragment array independently:

```sh
forge context compose --fragments fragments.json --spill-dir /tmp/context-fragments --max-bytes 8000
```

The JSON result includes the composed text, input/output bytes, omitted hashes,
duplicate count, and an overflow flag. Exit code 2 means required context or
recovery metadata exceeded the budget. `--max-tokens` uses UTF-8 bytes as a
conservative token upper bound, not an exact tokenizer or billing estimate.

## Actual provider usage

```sh
forge context report /path/to/context-events.jsonl --json
```

Existing context byte and tool overhead summaries remain available. The additive
`model_usage` section reports provider-supplied input, output, cached input,
cache-creation, and reasoning tokens. Unknown counters remain null, with field
coverage counts. Cache and reasoning subsets are not counted twice. Anthropic
cache counters are added to its uncached input count when those native fields
are present. Tool output byte estimates never become actual model tokens.
Provider-reported total-only usage remains a separate `reported_total_tokens`
counter; it does not imply known input or output counts. Request identity coverage
and complete input/output request counts are separate from the fraction of all
telemetry events containing usage.

Adapters collect explicit request/response IDs, task IDs, model names, caller
reported task outcomes, and optional latency/first-token timings. IDs are hashed;
prompt or response contents are not collected. Repeated records with the same
provider/session/request identity reconcile cumulative counters by maximum.
Unkeyed records cannot be deduplicated and are reported separately. Reported
validated-task counts are caller labels, not promotion or approval evidence.
Token cost per reported validated task covers observed requests only and remains
null when required counters or deduplication identities are missing.

Harness hooks that do not expose provider usage will show missing coverage.
Streaming changes when output arrives; it does not by itself reduce token usage.
Provider cache controls belong to the API client or harness. Keep stable policy
and tool prefixes unchanged and dynamic state at the end before measuring cache
hit rate and end-to-end task cost.

## Deferred tool definitions

The CLI accepts an MCP `tools/list` snapshot or a plain tool array:

```sh
forge context tools --catalog tools.json --query graph --detail names
forge context tools --catalog tools.json --query graph_context --detail schema --max-bytes 8000
```

Definitions are ordered deterministically, conflicting duplicate names are
rejected, and the serialized result respects its byte budget. Summary mode
includes descriptions; schema mode loads whole selected definitions. Omitted
results and incomplete MCP pagination are explicit. A changed catalog produces
a changed catalog hash.

This is an adapter primitive: a harness must expose discovery and fetch the
selected schemas to use it. Running the command does not change another
client's MCP catalog. Measure whether saved schema tokens outweigh the extra
discovery turn for your workload. Compose deterministic CLI/tool pipelines to
filter intermediate data locally before giving the final bounded result to the
agent.

Related controls: [graph context](graph-context.md), [messaging](messaging.md),
[local checks](local-checks.md), and [performance receipts](performance.md).
