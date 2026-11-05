# Native token counting and context virtualization

`forge context tokens|compact|recover` uses an offline Rust BPE implementation,
native syntax inspection and immutable recovery artifacts. The persistent harness
exposes the same operations. No model call, Python process or Node runtime runs.

## Prompt contracts and literal retention

`forge context prompt --request request.json` inspects explicitly supplied task
text without a model. For example:

```json
{
  "mode": "plan",
  "prompt": "# Objective\nFix the Rust planner's external-target handling.\n# Constraints\nDo not change the selected source set.\nPreserve explicit refusals for unsupported ownership.\n# Verification\nRun the targeted ownership regression tests.\n# Acceptance criteria\nThe requested planner path returns READY.\n",
  "constraints": ["Preserve unrelated working changes."],
  "max_output_bytes": 8192
}
```

- `inspect` returns ordered byte spans, hashes and categories for recognized
  sections or explicit requirement lines. It does not echo the full prompt.
- `suggest` adds optional clarification questions where no corresponding evidence
  was recognized; it does not assert that the information is absent or required.
- `plan` adds an ordered checklist referencing that evidence and an execution
  contract containing the original prompt and caller-supplied constraints exactly,
  including their order and duplicates. It is a checklist, not inferred dependency
  scheduling or authorization to execute anything.
- `compare` requires a nonblank `candidate` string and reports literal containment
  separately for the whole prompt, each caller constraint and each recognized
  non-objective evidence span. Objective and opaque spans have no individual
  retention check. A quoted or contradicted requirement can still
  be a literal match. Neither a match nor a nonmatch establishes semantic
  preservation, equivalence, correctness or instruction priority.

The lexical grammar recognizes known Markdown headings and colon labels for
objective/goal, constraints, acceptance, verification/tests and performance, plus
explicit requirement prefixes such as `must`, `do not` and `never`. Fenced,
blockquote, angle-block and quote-leading physical lines are recorded as opaque
context. Fence closers must match the opening character and length; trailing
nonwhitespace is not a closer. Quote-leading lines never consume later lines.
This is deliberately not a complete Markdown or natural-language parser. Unmarked
repository/tool text cannot be distinguished from other caller-supplied text;
the output never grants it instruction authority. Caller constraints remain
separate provenance records, not higher-priority policy. No automatic rewriting,
model refinement or execution is performed in this slice.

Requests reject unknown fields, explicit `candidate: null`, candidates outside
compare mode, blank inputs and unsupported modes. Limits are 512 KiB serialized
request, 64 KiB aggregate text (including next-action facts), 32 constraints and
256 evidence/opaque spans. Output budgets range from 512 bytes to 256 KiB; overflow
refuses rather than truncating protected content. The CLI emits compact UTF-8 JSON.
Its byte budget excludes transport framing and model tokens. Use the existing
pinned tokenizer operations when exact supplied-text token counts are needed.

The MCP tool is `context_prompt`, backed by the same Rust implementation and the
existing bounded context worker. It is advertised only with an operator context
capability whose byte budget is at least 512. That capability still requires its
configured tokenizer, although contract analysis itself does not tokenize. Raw
requests, aggregate text and output also obey the operator's byte limit. The MCP
front end first reads and validates the frame under its global 1 MiB bound;
the smaller operator limit applies in the context worker, not before every ingress
allocation. Existing transport/session limits remain independent. CLI/core parity and actual native
MCP stdio protocol tests cover discovery, calls, errors and absent capability;
these are not proof that a particular coding client selects the tool.

### Optional next inspection

When already preparing a `suggest` or `plan` request, callers can supply
`next_action` to obtain at most one bounded `code_inspect` recommendation:

```json
{
  "mode": "suggest",
  "prompt": "Inspect the reported Rust failure while preserving the API.",
  "max_output_bytes": 8192,
  "next_action": {
    "intent": "inspect_failure",
    "allow_read_advice": true,
    "path": "src/math.rs",
    "symbol": "parse",
    "recent_error": {"kind": "rustc", "exit_code": 101, "code": "E0308", "line": 12},
    "max_results": 20,
    "max_bytes": 2048
  }
}
```

The response's `next_action.call` names the existing MCP tool and its arguments.
The caller decides whether to execute it through the normal capability gate.
`allow_read_advice` is a caller filter, not an authorization grant. The same
request works through `forge context prompt --request FILE` and MCP
`context_prompt`; no extra discovery or recommendation command is introduced.
Call `code_inspect` directly when its location and arguments are already known.

The selector uses an explicit relative `path`, or the typed diagnostic's `path`.
Conflicting paths, a missing location or disabled read advice produce no call.
Diagnostic prose is opaque and cannot select an executable or grant permissions.
File existence, supported language and source freshness are not checked during
recommendation. Inspection returns signatures and source locations, not a
diagnosis or complete function bodies; bounded output can truncate or omit the
reported line. A diagnostic line is not an inspection filter.

An optional `source_sha256` and `known_inspection` record can suppress an
unchanged read. The record must declare `complete: true`, the same `path` and
`symbol` selector, and a matching lowercase SHA-256. These are caller-reported
facts; the response explicitly marks freshness as unverified. A partial or
failed prior read must not be reported as complete. This is not a source cache.

Omitting `next_action` preserves the previous output bytes. Explicit null,
unknown fields, traversal paths, unsupported intents, and use in `inspect` or
`compare` refuse. The recommendation allows 1–100 results and 512–8192 output
bytes; all supplied text counts against the existing input and operator budgets.
It adds no model call, filesystem read, execution, automatic retry or client hook.

## Tokenizer identity and counting scope

The operator supplies a local Hugging Face `tokenizer.json` and a configuration:

```json
{"path":"/absolute/tokenizer.json",
 "tokenizer_sha256":"<64 lowercase hex characters>",
 "model_sha256":"<64 lowercase hex characters>",
 "add_special_tokens":false}
```

Both hashes must be nonzero. The tokenizer file is bounded to 128 MiB, must be a
regular file and cannot be a final symlink. Its complete SHA-256 is verified
before parsing. The model hash records the operator's asserted association;
Forge does not load model weights or attest a serving process from this value.

The backend is pinned to [Hugging Face's Rust tokenizers 0.23.2](https://docs.rs/tokenizers/0.23.2/tokenizers/),
with native `fancy-regex` and no HTTP feature. Only BPE artifacts are accepted.
Stochastic dropout is rejected. Padding and truncation are explicitly disabled,
and BPE word-content caching is disabled. A fixed 256-slot SHA-256/count cache
retains no input text or token arrays; collisions evict after a full hash check.
The recipe fingerprint binds implementation,
artifact hash, model hash and special-token behavior. No implicit download occurs.

Counts describe the **exact supplied text through the pinned tokenizer pipeline**.
`add_special_tokens` controls the artifact's postprocessor once for the complete
text. An external chat template, role framing, multimodal inputs and generation
reserve are excluded unless already rendered into the supplied text or accounted
for explicitly. Independent fragment counts are not summed as the final answer:
the joined output, separators and recovery marker are encoded together.

```sh
forge context tokens --tokenizer tokenizer-config.json --input prompt.txt --ids
forge context compact --tokenizer tokenizer-config.json --request context.json \
  --spill-dir /private/recovery
forge context recover --spill-dir /private/recovery --sha256 HASH
```

Tokenization accepts at most 1 MiB of UTF-8. Operator artifacts and their regexes
and normalizers are trusted configuration, not model arguments. Encodings allocate
native token arrays; counting skips unused offsets. Instrumented warm hash/count
hits allocate nothing; uncached encoding allocates. Batch/Rayon encoding
APIs are unused. Large or expanding pipelines still consume CPU/memory, so the
persistent service enforces a cgroup ceiling and admits one context worker.

## Ordered context and protected content

```json
{"entries":[
  {"id":"policy","kind":"instructions","content":"Keep these instructions."},
  {"id":"build-call","kind":"tool_call","exchange":"build-1","content":"build request"},
  {"id":"build-result","kind":"tool_result","exchange":"build-1","content":"build result"},
  {"id":"current","kind":"text","content":"Continue fixing the compiler error."}
],"max_tokens":4096,"max_bytes":16384,"reserved_tokens":512,"keep_recent":1}
```

Entries remain chronological, with unique IDs and optional contiguous exchange
groups. Tool messages require an exchange ID; a result must follow a call in that
group. Entire groups are retained or omitted together. The newest `keep_recent`
groups are retained, as are `instructions`, `policy`, `approval`, `error` and
explicitly protected entries. These labels only control presentation retention;
they confer no approval or execution authority.

If the input fits, it is returned byte-exact with separators and no recovery
write. Otherwise Forge first persists the entire original request and tokenizer
identity. It then applies deterministic reductions:

- `code` entries require an explicit language. Native Tree-sitter folds function
  bodies while retaining signatures. Parse failures retain the original and report
  a reduction error. Projections are marked as context and must not be executed.
- `log` entries retain recognized Rust/C/C++ compiler errors/warnings, Cargo JSON
  diagnostics, nearby source/caret lines and a short tail. Recognized errors make
  their whole group required. Unknown formats are not inferred to be successful;
  every omitted line remains in exact recovery.
- `diff` entries retain headers and every changed line, replacing unchanged
  context with an explicit omission marker. This projection is not an applicable
  patch; recovery contains the original patch bytes.

Explicitly protected entries and protected categories are never transformed.
Other required groups may use projections but cannot be evicted. Old optional
groups are evicted in order until both rendered-byte and exact-token budgets fit.
Independent group counts guide eviction batches; final counts always use the
whole rendered text. This is deterministic sliding retention, not an optimal
packing algorithm or a learned summary.

If required content and recovery metadata cannot fit, `protected_overflow` is
true. The CLI exits 2 and returns the preserved output and counts. The harness
returns the same explicit flag; callers must resolve overflow before sending to
an inference endpoint. It never reports an over-budget projection as fitting.

## Recovery and bounds

Recovery roots must be absolute, owned mode-0700 directories. Files are immutable
mode 0400 JSON artifacts named by full SHA-256. Publication fsyncs a private
temporary file, atomically renames it, and syncs the directory. Existing artifacts
are verified and synced before reuse. Recovery verifies ownership, permissions,
link count and checksum; final symlinks are rejected.

Requests have at most 256 entries and slightly less than 1 MiB of content to leave
space for separators and recovery metadata. Each artifact is at most 8 MiB after
JSON escaping. The store defaults to 256 MiB and accepts at most 4096 files;
`--store-bytes` sets a bound up to 4 GiB. There is no automatic deletion of old
recoverable context. Quota, publication or checksum failure returns an error
without returning a reduced projection. A caller still owns its original input.

Same-UID unconfined programs remain trusted. Failed staging files left by abrupt
termination count against the quota until operator cleanup. Concurrent writers
get an explicit busy/retry error; readers see complete published artifacts.

## Persistent harness

Add an optional `context` capability to the operator policy:

```json
{"context":{"tokenizer":{"path":"/absolute/tokenizer.json",
 "tokenizer_sha256":"<64 lowercase hex characters>",
 "model_sha256":"<64 lowercase hex characters>","add_special_tokens":false},
 "max_input_bytes":262144,"max_tokens":32768,"recovery_bytes":268435456}}
```

`context_count` accepts text, `context_compact` accepts the request above, and
`context_recover` accepts `sha256`, `offset` and `limit` (1..65536 bytes). Recovery
returns base64 chunks, total bytes, next offset and EOF so large artifacts do not
overflow the transport. Recovery lives inside the private service state.

The capability caps input bytes at 256 KiB and token budgets at 262144. Admission
adds 128 MiB plus sixteen times the artifact size rounded to MiB to supervisor
reservations. This is an admission allowance; the service cgroup is the enforced
memory ceiling. Configure aggregate memory accordingly. Models cannot replace
the tokenizer, model identity, store path or operator limits through tool calls.

## Verification scope

Native tests compare a byte-level BPE fixture against an independent ranked-merge
control, including Unicode, special tokens, concurrency, disabled padding and
truncation. A separately pinned GPT-2 artifact passes known-ID and Unicode
round-trip checks. Its tokenizer SHA-256 is
`8414cab924d8b9b33013f0d221c5862f365ee9be39c5c2bfae8a5a9e970478a6`, from
[GPT-2 revision 607a30d](https://huggingface.co/openai-community/gpt2/tree/607a30d783dfa663caf39e06633721c8d4cfcd7e).
Tests never download artifacts; set `FORGE_GPT2_TOKENIZER` when opting into that
test. No model weights or inference job are involved.

Eight context tests cover exact preservation, groups, syntax, diagnostics, quota,
corruption, recovery, CLI exit status and 64 varied Unicode histories. All 13
live harness tests passed after the context capability was added, followed by
strict all-target Clippy checks. Six tokenizer tests now also cover bounded count
cache collisions and eviction; a separate allocation test observes zero
allocations over 100 warm successful count calls. These are correctness results, not
proof of downstream model task quality.

## Release measurements

On the shared Ryzen 9950X3D host with Rust 1.98.0, a single native caller and the
pinned GPT-2 tokenizer, 11 batches measured the following medians. Inputs contain
Rust source and Unicode. These are current-process costs, excluding service
transport and inference; warm repeats use identical input bytes.

| Operation | Input | Median |
|---|---:|---:|
| Load and verify tokenizer, warm filesystem | 1,355,256 bytes | 24.94 ms |
| Uncached offset-free count | 4 KiB | 694.2 µs |
| Warm hash/count hit | 4 KiB | 1.50 µs |
| Uncached offset-free count | 64 KiB | 11.33 ms |
| Warm hash/count hit | 64 KiB | 23.26 µs |
| Uncached offset-free count | 1 MiB | 263.1 ms |
| Warm hash/count hit | 1 MiB | 372.7 µs |
| First context compaction, including recovery fsync | 212,213 bytes | 63.47 ms (one sample) |
| Repeated compaction, verified recovery reuse | 212,213 bytes | 2.65 ms |

The compaction fixture reduces 57,430 input tokens to 3,277 rendered tokens plus
a 512-token reserve, dropping 29 old groups. Before the count cache, the same
repeated fixture measured 75.87 ms. This improvement concerns repeated inputs;
new text still pays the uncached BPE cost. The benchmark observed one thread,
93,200 KiB final RSS and 216,304 KiB peak RSS across all workloads, including the
1 MiB case and tokenizer reloads. It does not isolate allocator-retained memory.

Run `FORGE_GPT2_TOKENIZER=/absolute/tokenizer.json cargo +1.98.0 bench --locked
--manifest-path native/forge/Cargo.toml --bench context`. Measured benchmark
binary SHA-256: `712834dc9c474b9d11922678eab836ece35e75d3ab4b6d7c9643e855e0c42a05`;
benchmark source: `180571d94f89e1aaceb4ef11ea98154aab5c8e93ba5653ebb860face949d8b6c`;
tokenizer wrapper: `a1f919b38dc19c12f5a6e3830d730839738e45bce2ede8a168d2cb13d965cbdc`.
The pre-cache benchmark binary was
`b887941e6bd71ab88508b66f5a1f89dd4a354b0b8b0043903fce810ab690f915`.

## Native session handoff

`forge handoff --host REPO stage --identity NAME --task "Next step"
--context-file notes.txt` prepares `.agents/handoff/NAME/pending.json`.
`forge handoff --host REPO hook --identity NAME` emits SessionStart JSON and
activates it once. `prepare --output FILE` and `activate --input FILE` provide
the explicit-file workflow. All commands run without an interpreter or PATH.
The native aggregate SessionStart hook invokes the same handoff command. See
[native-lifecycle.md](native-lifecycle.md) for composition and stage limits.

Version 1 envelopes retain the canonical sorted-JSON SHA-256 contract. Activation
revalidates live mandates, headings and unexpired claims; only the snapshot's
volatile `last_updated` field is excluded from its fingerprint. A changed state,
invalid integrity hash, duplicate JSON key, unsafe file or busy writer produces an
explicit error. Hook activation errors become visible context and preserve the
staged file. No handoff changes a claim, grants approval or replaces a conversation.
The hash detects corruption; it does not authenticate against a trusted same-UID
writer who can recompute it.

The envelope is capped at 1 MiB, task at 500 characters, context at 3,500 characters,
and metadata at 128 relative paths and 64 scalar runtime fields. Context redaction
and character clipping retain the legacy handoff contract. Use the recovery-backed
`context compact` operation above when exact long-context recovery is needed.
Without explicit context, preparation uses task, paths and native state projection;
it performs no implicit knowledge search or whole-source reads. Identity resolution
uses `--identity`, `A2A_AGENT_NAME`, `.agents/a2a/default_identity`, then `claude`.

Writers hold a nonblocking sibling lock across parent-lineage reads and publication.
Owned, singly linked regular files are required; final symlinks, public envelope
permissions and special files are rejected. Staging files are created exclusively
with mode 0600, synced, renamed and followed by directory sync. Parent directories
are operator-trusted. Three native unit tests and six CLI tests cover state and
integrity failures, UTF-8 clipping/redaction, lineage, repeated activation, lock
contention, symlink/hardlink rejection and empty-PATH execution.
