# Native structural code graph

`forge graph index` parses Python and Rust source in a host checkout and writes
`.forge/graph.db`. It uses Rust parsers and SQLite; it does not start Python,
an embedding model, or a GPU job.

```sh
forge graph --host /path/to/project index
forge graph --host /path/to/project context pkg/module.py --symbol function_name
forge graph --host /path/to/project refs --text 'Review pkg/module.py::function_name'
```

`index` returns JSON counts for files, definitions, resolved calls, unresolved
calls, and dynamic calls. Its `authority` is
`forge-native-structural-graph`, and `embeddings` is `absent`. A resolved
`CALLS` edge is created for an unambiguous local definition, an explicitly
imported Python definition, or a Rust `crate::module::function` path that maps
to one indexed file. Dynamic calls, ambiguous definitions, and references that
cannot be resolved by these rules have no edge; their counts appear in the
index report. Cross-language calls and semantic similarity are not inferred.

`context` returns a bounded source excerpt, indexed symbols, callers, callees,
and `graph_status`. It compares the queried file's current SHA-256 with the
index and clears its relationships when the file has changed. Re-run `index`
after edits or deletions to refresh relationships throughout the graph. `refs`
finds up to a small number of concrete code references in a message and uses
the same graph projection.

The default index is a complete SQLite snapshot built in a temporary file in
the destination directory, then atomically renamed into place. A source read
or parse failure leaves the previous database intact. Reindexing removes
deleted files and their edges. The scanner skips symlinks, hidden directories,
`target`, `node_modules`, `__pycache__`, `venv`, `build`, and `dist`; it accepts
at most 30,000 Python/Rust files of at most 1 MiB each, 128 MiB of source in
total, 100,000 definitions, and 1,000,000 static calls. Invalid UTF-8, NUL
bytes, oversized source, and syntax errors stop the refresh with a file-specific
error.

Use `--db` to select a database path explicitly for either indexing or queries:

```sh
forge graph --host /path/to/project --db /tmp/project-graph.db index
forge graph --host /path/to/project --db /tmp/project-graph.db context pkg/module.py
```

Without `--db`, queries prefer `.forge/graph.db`. If it is absent, they read
an existing `.code-review-graph/graph.db` for compatibility. Indexing never
writes that external database by default. An explicit `--db` path is the
destination the caller has chosen, so use a separate path when preserving an
existing index.
