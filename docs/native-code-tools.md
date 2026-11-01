# Native source tools

`forge code --root REPO inspect FILE` reads a bounded, root-relative UTF-8 file
and reports definitions, signatures, lexical scopes, references and dependency
spellings. Rust, Python, C, C++, JavaScript, TypeScript and TSX use compiled
Tree-sitter grammars. `--language cpp` selects C++ for an ambiguous `.h` header.
`--symbol` selects a simple or scope-qualified name; all ambiguous matches remain
visible. Source spans are byte offsets and one-based line numbers, and every
response includes the source SHA-256. These are syntax facts, not type-checker
proof of reference identity, macro expansion or dynamic dispatch.

`forge code fold FILE` replaces function bodies with syntactically valid stubs
and marks the result `context_only`. It never changes the file. Never execute a
folded projection. `--max-bytes` bounds response size; inspection additionally
accepts `--max-results`. Resource limits reject excessive source, syntax depth,
nodes, parsing time or accumulated output rather than silently omit facts.

`forge graph index` shares the language registry. Existing Python and Rust
dependency analysis remains in use. C/C++ include paths and JS/TS relative import
paths produce conservative direct edges, while local lexical calls connect
definitions. Native-language dependency coverage remains explicitly incomplete:
build include directories, macros, link inputs, bundler aliases and runtime
behavior cannot be inferred from syntax alone. The test selector broadens coverage
when it encounters this uncertainty.

## Transactional edits

Write a JSON request and run `forge code --root REPO patch --request REQUEST` to
prepare it without changing source. Add `--apply` to commit the prepared change.
For example:

```json
{
  "path": "src/lib.rs",
  "expected_sha256": "<complete SHA-256 of current file>",
  "edit": {
    "kind": "replace",
    "search": "return 42;",
    "replacement": "return 43;",
    "expected_matches": 1
  }
}
```

Search replacement requires the exact requested match count (default one).
Alternatively use `{"kind":"unified_diff","patch":"...","max_offset_lines":0}`.
Unified diffs address one existing file, verify hunk counts and file headers,
preserve newline bytes, and reject overlapping hunks. An explicit offset budget
of up to 4096 lines permits location recovery only when exact context has a
unique match. It never guesses whitespace or changes unmatched content.

Preparation rejects stale hashes and validates the candidate with its native
language grammar, JSON parser or TOML parser. Unknown extensions receive UTF-8
validation, labelled `utf8_text` in the response. Use a language override when
extension detection cannot identify source. Syntax validity does not prove type
correctness or behavioral equivalence; run the relevant compiler and tests.

Application takes a root-directory advisory lock, checks source identity and
content again, retains the exact old bytes as a native Git blob under
`refs/forge/undo/<blob>`, writes and syncs a sibling temporary file, checks again,
atomically renames it, and syncs its parent. A reported parent-sync failure
attempts to restore the old bytes if no intervening content change is detected.
Preparation changes no files; application changes the selected file and Git
recovery metadata only. Permissions are preserved. Multiply-linked files are
rejected because replacing one name would detach it from its aliases.

Recover with `forge code restore FILE --expected-hash CURRENT_SHA256
--git-snapshot BLOB`, adding `--apply` after preparation. Recovery also requires
the current hash and retains the version it replaces. Recovery references keep
blobs reachable by Git garbage collection; they are not branches. No command
resets the index or checkout. No Git executable or interpreter is needed.

The selected root and its Git metadata must be trusted. File reads use directory
capabilities and reject traversal, Git metadata paths, final symlinks and
nonregular files. Application requires an ordinary local `.git` directory;
external Git-directory pointers are rejected. Advisory locking coordinates Forge
writers. An unrelated editor that ignores the lock still has a narrow race
between the final comparison and rename; this is not a filesystem compare-and-swap
primitive. Atomic replacement prevents partial source writes, but a machine power
loss can still affect unsynced Git metadata. Interrupted preparation may leave a
hidden sibling temporary file; it cannot replace the selected source itself.

The source limit is 1 MiB. Diff matching has a 32 MiB comparison budget, 1024
hunks, and bounded line offsets. Inspection allows 200,000 nodes, depth 256,
8 MiB of accumulated text, and 250 ms each for parsing and traversal.

## Background syntax refresh

`forge graph --host REPO refresh` durably queues a native syntax refresh and
returns immediately. `forge graph --host REPO wait --timeout-seconds 10` waits
for completion and exits nonzero on timeout or a persistent refresh failure.
Edit/delete and Git tree-rewrite hooks use the same queue. No Python, external
`code-review-graph`, Git executable or embedding provider is launched. Repository
revision lookup uses libgit2. Semantic indexes have their own explicit
operator-configured [refresh path](native-code-search.md).

The worker builds `.forge/graph.db`, reuses unchanged syntax facts and atomically
publishes a synced complete database. It does not alter the external graph's
database. Compatibility markers remain in `.code-review-graph` or `CRG_DATA_DIR`;
a relative override is resolved before the worker changes its current directory.
State directories/files must be owned and not writable by other users or groups.
Final symlinks and multiply linked state files are rejected. Parent directories
and unconfined processes belonging to the same UID remain trusted.

One worker inherits an already-held flock, preventing duplicate spawns. Requests
arriving during a batch remain queued. The pending marker is renamed to inflight
and synced before the child starts, so interrupted work can be recovered. Each
worker has a 300-second lifetime, 250 ms debounce and a native child process group
whose deadline and 64 KiB output bound are enforced by kernel readiness. Queue
data above 256 KiB coalesces to one full-refresh marker. Query waits use inotify
and poll rather than periodic filesystem scans. No long-lived service is installed.

A failed build preserves the previous graph, records bounded notices, and leaves
a persistent stale marker. Default graph context/refs queries wait for pending
work and reject a known stale generation, including when the requested source
itself did not change. Retry with `forge graph refresh`; a successful batch clears
the stale marker. Explicit `--db` reads remain an operator-selected compatibility
path. The source tree is not an atomic filesystem snapshot: unrelated editors
must still coordinate writes, and syntax edges do not establish type correctness.

Six native integration tests cover empty-PATH refresh, changed/deleted sources,
concurrent requests, interrupted-marker recovery, failed-build preservation,
stale-query rejection, relative state paths, locks and unsafe files. The existing
27 graph-index tests and 49 post-tool/unit corpus tests also passed at this stage.
