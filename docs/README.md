# llm-forge platform reference

llm-forge ships governance mechanics — claims, leased worktrees, a single landing gate,
automated mutation evidence, and context/token budget discipline — as executable tooling
under `src/conductor/`. This directory documents the policies those tools can support
when a host explicitly adopts them. Shipping these tools does not activate their
policies in Forge. The LLM repository's governance requirements apply only to LLM;
Forge uses ordinary tests and code-quality checks without mandatory governance claims,
landing-gate receipts, or mutation evidence.

Each page keeps its **KB id** (`KB-GOV-01`, `KB-MUT-02`, …) as a stable anchor: the code
and doc comments under `src/` cite these ids directly (`grep -rn 'KB-' src/` finds every
citation), so the id — not the filename or heading text — is the thing that must never
move. If you rename a page, keep the id in its front matter and its `#` heading.

## Pages

| Id | Topic | Page |
|---|---|---|
| `KB-GOV-01` | Governance claims, worktree isolation, mandatory context checks | [KB-GOV-01.md](KB-GOV-01.md) |
| `KB-GOV-02` | How work lands: branches, the one gate, attributed findings, evidence | [KB-GOV-02.md](KB-GOV-02.md) |
| `KB-GOV-06` | Risk rating and approval authority (rate the work, then act) | [KB-GOV-06.md](KB-GOV-06.md) |
| `KB-GOV-07` | Landing from a fresh worktree: the concrete gate sequence | [KB-GOV-07.md](KB-GOV-07.md) |
| `KB-MUT-02` | Automated-only mutation evidence contract | [KB-MUT-02.md](KB-MUT-02.md) |
| `KB-OPS-CTX-01` | Context and token budget, inbound and outbound | [KB-OPS-CTX-01.md](KB-OPS-CTX-01.md) |
| `KB-CI-01` | CI coverage: a tree nothing runs is a tree nothing checks | [KB-CI-01.md](KB-CI-01.md) |

Every id cited anywhere under `src/` (`KB-GOV-01`, `KB-GOV-07`, `KB-MUT-02`, `KB-CI-01`) has
a page above. `KB-GOV-02`, `KB-GOV-06` and `KB-OPS-CTX-01` are not yet cited by any source
comment but document the same optional host policies (branch/landing discipline, approval
tiers, and context-budget rules) and are included for the same reason the cited ones are.

These pages are deliberately host-agnostic: no training runs, no GPU-specific rules, no
corpus or model names, no personal names, and no absolute paths from the monorepo this
platform was extracted from. Where a card originally cited a concrete Makefile target or
CLI flag that does not (yet) ship in `llm-forge` itself, the page describes the mechanic and
names the `conductor.*` primitive it is built from instead of inventing a command that does
not exist here.

## Wiring a host project's own notes

Native `forge cards` and `forge memory` provide knowledge-card and source-memory
indexing and retrieval (`KB-OPS-CTX-01`, "retrieve before re-deriving"). The
Python `conductor.kb_retrieve`, `conductor.memory_index`, and
`conductor.index_notes` adapters have been retired. Native notes configuration
uses `CONDUCTOR_NOTES_ROOT` or `notes_root` in `[conductor]` of `forge.toml`
(legacy `[tool.conductor]` in `pyproject.toml` is also supported), defaulting
to `research/notes`. Other Python host tools that still read notes use
`conductor.project_paths` with the same configured root. Memory source selection
uses the host's `memory_sources` catalog. See
[`native-memory-ingestion.md`](native-memory-ingestion.md) for memory and card
contracts.

Cards are complete top-level Markdown files matching `kb_*.md` or `KB-*.md` in
the notes root. This repository configures `notes_root = "docs"`, so its law
pages form its card collection. Build and search a card index with an explicitly
pinned embedding configuration:

```sh
forge cards build --host /path/to/host --output /private/cards.json \
  --embedding /private/embedding.json
forge cards search --index /private/cards.json --embedding /private/embedding.json \
  --task "mutation campaigns" --top-k 3
```

For broader source memory, ingest and query an explicit private store:

```sh
forge memory ingest --host /path/to/host --store /private/memory-store \
  --embedding /private/embedding.json
forge memory search --store /private/memory-store --embedding /private/embedding.json \
  --task "compiler diagnostics" --top-k 8
```

The separate prose-search index uses the configured `notes_db` path (default
`research/notes.db`). Its SQLite file must already exist before indexing; then
it can be rebuilt or queried natively:

```sh
forge notes index --host .
forge notes search --host . --json "mutation campaigns" --limit 3
forge notes tables --host . --json "native memory"
```

See [`native-notes-index.md`](native-notes-index.md) for index sources, table
filtering, output fields, and database safety requirements.

A host configured with the monorepo's `research/notes` (or with nothing, which means the
same thing) keeps resolving there unchanged.

## Reference pages

| Page | Topic |
|---|---|
| [makefile_targets.md](makefile_targets.md) | Every `conductor.mk` target with its help text (generated — regenerate, do not hand-edit) |
| [bootstrap.md](bootstrap.md) | Scaffolding a host project with `python -m conductor bootstrap` |
| [native-notes-index.md](native-notes-index.md) | Native FTS and Markdown-table index commands |

## Top-level index

See the [project README](../README.md) for how this package fits into a host project;
`AGENTS.md` at the repository root contains Forge's own contribution rules. The host
policy references in this directory do not add governance requirements to that contract.
