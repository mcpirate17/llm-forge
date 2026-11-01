# Native structured output

`forge schema` compiles a bounded JSON Schema profile in Rust and validates
arguments without allocating on the heap. The library exports
`forge::tool_schema::{CompiledSchema, ToolRegistry, ValidatedCall}`. Registration,
GBNF generation, CLI I/O and reporting can allocate. No Python, Node, GPU or model
process is needed for these commands.

## Use

Save an argument schema as `inspect.schema.json`:

```json
{
  "type": "object",
  "properties": {
    "path": {"type": "string", "minLength": 1},
    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
  },
  "required": ["path"],
  "additionalProperties": false
}
```

```sh
printf '%s\n' '{"path":"src/main.rs","limit":20}' |
  forge schema validate --schema inspect.schema.json
forge schema grammar --schema inspect.schema.json > inspect.gbnf
```

`validate` also accepts `--input FILE`. Successful responses contain `valid:true`,
`repaired` and the exact JSON value. Invalid data returns `valid:false`, an error
code and byte offset, and exit 1. Invalid registration or I/O failure exits 2.

`--repair-fences` permits one repair: remove a sole Markdown JSON or unlabelled
fence with LF line endings, ignoring surrounding JSON whitespace. The extracted
JSON must pass the same validator. No fields,
values, quotes, types or commas are changed. Errors refer to the original input's
byte offsets. The library exposes this operation without allocating.

For persistent calls, save a complete catalog as `tools.json`:

```json
{"tools":[{"name":"inspect","description":"Inspect a path","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}]}
```

```sh
forge schema grammar --catalog tools.json > tools.gbnf
printf '%s\n' '{"name":"inspect","arguments":{"path":"src/main.rs"}}' |
  forge schema serve --catalog tools.json
```

The server compiles the catalog once, reads one call per JSONL record, writes one
response per record and flushes promptly. Bad, oversized or invalid UTF-8 records
produce an error response; the next record is still processed. It retains at most
1 MiB of record data. EOF ends the server; its exit status describes transport
success, while each response's `valid` field describes the call. JSONL cannot
contain raw multiline fenced documents; use the single-document command or
library repair API for those. Pipe closure propagates an I/O error.

Tool names and argument schemas are closed at registration. Unknown tools,
duplicate envelope fields, extra envelope fields, stringified argument objects
and invalid signatures are rejected. Only `ToolRegistry` constructs
`ValidatedCall`; native handlers can require that type and borrow its name and
arguments. **The CLI does not execute tools.** Existing host dispatchers do not
automatically acquire this boundary. Handler wiring is a later architecture
milestone, and host permission checks remain required for their existing scopes.

## Supported schema profile

Every schema node needs one explicit string `type`. Supported types are object,
array, string, number, integer, boolean and null. Objects require explicit
`additionalProperties:false`; `properties` and `required` are supported. Arrays
require a single `items` schema and support `minItems`/`maxItems`. Strings support
`minLength`/`maxLength` (Unicode scalar counts), string `enum`, and string `const`.
Numbers support inclusive `minimum`/`maximum` with exact decimal comparison.
`integer` accepts mathematically integral JSON numbers such as `1.0` and `1e2`.
No floating-point conversion decides a bound or integer constraint.

`$schema`, when supplied, must identify JSON Schema 2020-12. `title`, `description`,
`default`, `examples` and `deprecated` are non-enforcing annotations; defaults
are never inserted. All other keywords fail registration, including `$ref`,
`oneOf`/`anyOf`, `pattern`, `format`, `multipleOf`, schema-valued additional
properties and type unions. This is explicitly a tool-call profile, not a
complete JSON Schema implementation. Schema documents themselves reject
duplicate decoded keys before DOM parsing can overwrite them.

Limits: 1 MiB per input/schema/catalog, 32 schema/value descents, 4,096 nodes per
schema, 128 properties per object, 256 tools per catalog, 256-byte property/tool
names, 256 string enum choices of at most 4,096 bytes each, and explicit string
or array length bounds of at most 4,096. Unspecified maximum lengths remain
bounded by the input size. JSON number exponents must be within ±1,000,000.
These are explicit resource limits; some otherwise valid JSON/schema documents
therefore cannot register or validate. Catalog pages must be assembled before
registration; partial catalogs and duplicate tool names are rejected.

## Grammar contract

GBNF emits deterministic sorted object fields and canonical JSON encodings for
names and enum choices. Generated whitespace is spaces/tabs so complete calls
fit JSONL records; the validator also accepts ordinary JSON newlines.
Optional fields use a linear sequence of grammar states,
not a factorial expansion of object orderings. Validation accepts arbitrary
property order and equivalent Unicode escapes. Generated string productions
handle escaped BMP scalars and paired UTF-16 surrogates; lone surrogates fail.
Array/string length and string enum constraints are included in the grammar.
Explicit string bounds support up to 1 MiB of Unicode scalars, subject to the
independent 1 MiB input-byte limit. Large repetitions use factored power-of-two
rules rather than a large expansion in the consuming grammar engine. Array
item bounds and literal enum strings retain their 4,096 limits.

The grammar is a **generation aid**, not a substitute for validation: numeric
bounds are checked at the native boundary, and numeric generation uses canonical
integer syntax and at most six exponent digits. A grammar-conforming number can
still violate a numeric bound. Always validate a complete model response before
dispatch, including when a backend claims constrained decoding support.

Semantics follow the [JSON Schema object reference](https://json-schema.org/understanding-json-schema/reference/object).
The grammar targets [llama.cpp GBNF](https://github.com/ggml-org/llama.cpp/blob/master/grammars/README.md).
No external grammar engine is required by the shipped binary.

## Validation and measurement

```sh
cargo test --release --locked --manifest-path native/forge/Cargo.toml \
  --test tool_schema --test tool_schema_allocations --test tool_schema_cli
cargo bench --locked --manifest-path native/forge/Cargo.toml --bench tool_schema
```

The allocation test instruments the system allocator after registration,
including Unicode escapes, invalid inputs, fence repair and registry lookup.
Stress checks include eight concurrent readers and 20,000 deterministic malformed
string cases compared with serde_json. Benchmarks report per-operation batch
means across 31 batches of 10,000 calls after warmup, rather than claiming
individual-call tail latency. Compilation is reported separately.

An optional interoperability test uses llama.cpp's real `test-gbnf-validator`
(build its CMake target with CPU support, without a model):

```sh
FORGE_GBNF_VALIDATOR=/path/to/test-gbnf-validator \
  cargo test --release --locked --manifest-path native/forge/Cargo.toml \
  --test tool_schema_grammar -- --ignored
```

It checks all subsets of five optional fields, nested collections, required
fields, lengths, escapes, enums and full catalog envelopes. This dependency is
test-only. Inspect its textual verdict: upstream returns exit 0 even for rejected
input. Development results and remaining milestones are recorded in
[ARCHITECTURE.md](../ARCHITECTURE.md).
