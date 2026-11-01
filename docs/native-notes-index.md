# Native notes index

The Forge binary provides a native FTS5 and Markdown-table index for a host's
notes and task documents. It replaces the indexing and query interface in
`conductor.index_notes`. The Python module and its compatibility target were
retired after the old-vs-native parity check passed. The index is a rebuildable
search cache, separate from run and experiment databases.

## Commands

```text
forge notes index --host HOST [--vault DIR] [--db PATH]
forge notes search --host HOST [--db PATH] [--limit N] [--json] QUERY...
forge notes tables --host HOST [--db PATH] [--limit N] [--json] FILTER...
```

The default database path comes from the host's `CONDUCTOR_NOTES_DB` or its
`notes_db` configuration (`[conductor]` in `forge.toml`, with legacy
`[tool.conductor]` in `pyproject.toml` also supported). The default is
`research/notes.db`. `--db`
overrides it for any command. `--host` is the host checkout root. The database
file must already exist as a singly linked regular file no larger than 4 GiB;
`index` does not create the file or its parent directory. The writer requires
current-user ownership, rejects symlinks in the final path and its ancestors,
preflights existing SQLite journal/WAL/SHM sidecars with the same ownership,
regular-file, single-link, and 4 GiB checks, and rechecks the database file
identity before commit. It admits only the expected legacy/native `notes_fts`
and `note_tables` schema definitions, normalizing ASCII whitespace, case, and
optional `IF NOT EXISTS`. It refuses ordinary-table impostors, nonstandard FTS
options or constraints, and custom triggers on the index tables or FTS shadow
tables. Rebuilds of `notes_fts` and `note_tables` run in one SQLite transaction
and print the indexed note and table counts.

SQLite uses a 250 ms busy timeout and VM execution timers of 2 seconds for
read-only connections and 30 seconds for the writer, plus a 2 MiB page cache.
These bound SQLite work, not total process I/O. Parent directories and
unconfined processes running as the same user remain trusted; this is not a
custom VFS or full hostile-race isolation boundary.

The index includes Markdown files under the configured notes root and the
host's `tasks/` tree, excluding `tasks/audit/`. `--vault` defaults to
`~/Documents/CodexVault`. If its `research/` directory exists, the index also
includes the vault's `research/`, `dashboards/`, and `runbooks/` trees. Entries
within the host retain host-relative paths; an external vault uses vault-relative paths.
The command refuses unsafe symlinked descendants, unreadable/non-UTF-8 or
oversized notes, and configured sources that disappear after they have been
indexed; failed rebuilds leave the previous index transaction intact.
The default scan is bounded to 100,000 examined entries, 10,000 Markdown files,
2 MiB per note, and 128 MiB total note text.

## Search

`QUERY...` words are joined with spaces. Each nonempty whitespace-separated
term is quoted and escaped for FTS5, with the terms matched together. Search
returns at most 20 results by default; `--limit` accepts 1 through 1000. Human
output shows the note title, relative path, and a bounded FTS snippet. `--json`
prints an array of `{path, title, snippet}` objects. An empty query returns no
hits. Queries are limited to 4096 bytes.

## Table lookup

`FILTER...` words are joined with spaces and matched with SQLite `LIKE` against
the indexed note basename (ASCII case-insensitive by default; `%` and `_` are
wildcards). Omitting the filter lists all indexed tables. Results are ordered
by note name, table index, then row id; `--limit` accepts 1 through 1000 and
defaults to 20. Human output shows the note, table number, row and column
counts, section heading, and column names. It does not print the table body.
`--json` returns an array of objects with `note`, `table_idx`,
`section_heading`, `n_cols`, `n_rows`, and `headers` fields. Output is bounded
to 1 MiB; narrow the filter or limit if that bound is exceeded. The filter is
limited to 4096 bytes.

## Configuration and compatibility

The notes tree and index path use the same `conductor.project_paths` rules as
other host configuration. `CONDUCTOR_NOTES_ROOT` / `notes_root` selects the
notes tree; `CONDUCTOR_NOTES_DB` / `notes_db` selects the database. Search and
table listing read an existing index; run `forge notes index` to rebuild or
refresh it.

Before retirement, the focused `python_contracts_index_notes` compatibility
target passed 2 tests across 4 fixture runs against a freshly built native CLI.
The native notes suite passed 15 unit tests and 14 CLI tests. Twenty additional
repetitions of the concurrent rebuild/read test passed, exercising 320 competing
rebuild attempts and 160 search reads, plus successful retry and final integrity
checks. These are the notes-specific results; they are not a full repository gate.
