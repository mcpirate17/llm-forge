# Native local clerk

The native clerk is an explicitly requested, bounded source inventory and
clerical-draft tool. It does not review, approve, authorize, attest to a model
server, or write source files. A generated document is labelled for human
review; its model name is descriptive provenance, not proof of which model
actually served the request.

## Commands

```text
forge clerk --host HOST inventory PATH...
forge clerk --host HOST draft PATH... [--model qwen3.5:9b] [--output RELATIVE_PATH]
forge clerk --host HOST validate PATH
```

`inventory` reads only explicitly supplied sources. `draft` sends the bounded
source envelope to the local Ollama endpoint and returns a document on stdout.
With `--output`, it writes under `research/tmp/local_clerk/`; relative output
paths are relative to that directory, and
publication uses exclusive creation so an existing file is never replaced.
Absolute outputs must also stay below that directory. A read-only destination
preflight runs before inference; publication repeats its checks afterward.
Output paths reject NUL and components over 255 bytes before filesystem access,
including when parent directories do not exist. Individual filesystems may
impose stricter limits; preflight does not reserve a destination.
`validate` re-reads and verifies the saved document and its source provenance.
Verification tests use native HTTP fixtures and do not load a model.

The host directory is always an allowed source root. Default additional roots
are `$HOME/.claude/tasks`, `$HOME/.codex/memories`, and
`$HOME/Documents/CodexVault`. Absent `HOME` leaves the host as the only default
root; explicit tilde paths require `HOME`. `CONDUCTOR_CLERK_ROOTS` is a
colon-separated replacement list; an empty value selects only the host. Missing
optional roots are skipped without creating them; malformed settings fail.
Sources, validation paths and configured roots resolve relative to the host.
The native core itself does not read process environment;
the CLI resolves host, home, and environment settings before calling it.
Direct ingestion of `.current_work.md` is forbidden.

Inference requires `LOCAL_AI_TASK=notes|summary|organization|compaction`.
`OLLAMA_HOST` defaults to `http://127.0.0.1:11434`; only plain origins on
`127.0.0.1`, `::1`, or `localhost` are allowed. `localhost` is pinned to
127.0.0.1 without DNS. Credentials, redirects, proxies and remote hosts are
rejected or disabled. The endpoint must report the exact requested model label;
this is a consistency check, not model artifact attestation.

## Bounds and source safety

- At most 3 distinct sources; each file is at most 64 MiB and the combined
  admission limit is 128 MiB.
- Source reads retain at most the first 64,000 bytes, decode lossily, and keep
  at most 2,400 Unicode characters per source. The rendered prompt is at most
  4,000 characters; it must retain a minimum excerpt for every selected source.
  Character limits do not attest the model tokenizer's token count.
- The only admitted model label is `qwen3.5:9b`. HTTP is plain loopback only,
  with request and response bodies capped at 64,000 bytes and an HTTP exchange
  deadline no greater than 120 seconds. These limits do not bound source reads,
  prompt construction, or final provenance revalidation to that HTTP deadline.
- After the HTTP exchange, every source is rehashed before the draft is
  returned; the CLI revalidates it again before exclusive publication. A
  changed source invalidates the result.
- Saved documents are limited to 256,000 bytes and are validated against the
  current source contents. Validation does not contact Ollama.

Pinned directory descriptors and no-follow opens reject source or output-root
replacement at checked boundaries. Newly created output directories use mode
0700 and files 0600; concurrent writers cannot replace an existing destination.
The operator controls ancestor directories and the mount namespace. These checks
do not provide an OS sandbox against an unconfined process with the same UID.
Filesystem reads have byte admission limits, not an independent wall-clock deadline.

Focused release validation passed 369 native tests across the Forge library,
clerk CLI and affected hook suites. Coverage includes source replacement during
inference, full-file provenance, concurrent publication, and zero HTTP requests
for invalid output destinations. Strict all-target Clippy passed. The native
AST scan retained every definition in the 12 inspected files; no function
exceeded 79 lines. The native discovery suite passed 33 tests, and the changed
optional compatibility selection case passed through a private binding overlay.
Published on `main` at `5971c1c6d41d3026faec5a6c37f77cc96aadb22b`.
Full checks passed before and after publication: 2,406 release tests and two
installed-runtime tests, with 39 intentional ignores. The post-publication
receipt SHA-256 is
`fa6c43f9ee678ee7de412521e409764025882ccda90801ff0e6a2cfc4d7cc2c5`.

The current-only Rust 1.98.0 release benchmark measured three 2 KiB sources at
27.252 microseconds median batch mean (p95 29.203), and one fully hashed 1 MiB
source at 404.523 microseconds (p95 406.768), approximately 2,472 MiB/s at the
median. The first workload used 31 batches of 100 operations after 100 warmups;
the second used 31 batches of 10 after 10 warmups. Filesystem caches were warm
and the host was not isolated. Timings include source reads, hashing, path
checks, bounded snippets and inventory construction; they exclude setup, JSON
serialization, process startup and inference. This is not a historical speedup
comparison. Benchmark executable SHA-256:
`23f057498aeeba632f8bfdf1e5662163ff2a068eeead802043fe51b1e25a2ad5`.
