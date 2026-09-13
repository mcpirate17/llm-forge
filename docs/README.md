# llm-forge platform laws

llm-forge ships governance mechanics — claims, leased worktrees, a single landing gate,
automated mutation evidence, and context/token budget discipline — as executable tooling
under `src/conductor/`. This directory is the reference documentation for the *rules*
behind that tooling, written for a host project adopting the platform rather than for the
monorepo it was originally extracted from.

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
comment but are part of the same platform contract (branch/landing discipline, approval
tiers, and the context-budget rules the retrieval tooling in this repo exists to enforce)
and are documented here for the same reason the cited ones are.

These pages are deliberately host-agnostic: no training runs, no GPU-specific rules, no
corpus or model names, no personal names, and no absolute paths from the monorepo this
platform was extracted from. Where a card originally cited a concrete Makefile target or
CLI flag that does not (yet) ship in `llm-forge` itself, the page describes the mechanic and
names the `conductor.*` primitive it is built from instead of inventing a command that does
not exist here.

## Wiring a host project's own notes

`conductor.kb_retrieve` and `conductor.memory_index` are the tooling this platform ships to
retrieve knowledge cards and indexed notes at query time (`KB-OPS-CTX-01`, "retrieve before
re-deriving"). The notes tree resolves through `conductor.project_paths` like every other
host path: the `CONDUCTOR_NOTES_ROOT` environment variable first, then the
`[tool.conductor]` `notes_root` key in the host's `pyproject.toml`, then the
`research/notes` default. Every reader — `kb_retrieve.load_cards`, `memory_index`'s
catalog (whose `root = "research/notes"` spelling means "the notes root", wherever the
host put it), the `check_json_in_notes` guard, `dead_tests`'s notes corpus,
`index_notes`'s fallback source and `snapshot_worktree`'s exclusions — resolves through it
at call time, never from a module constant.

Cards are files matching `kb_*.md` or `KB-*.md` in the notes root. This repository
configures `notes_root = "docs"`, so its own law pages are its knowledge base:

```sh
uv run python -m conductor.kb_retrieve index        # embed the KB pages
uv run python -m conductor.kb_retrieve query "mutation campaigns" --top-k 3
```

A host configured with the monorepo's `research/notes` (or with nothing, which means the
same thing) keeps resolving there unchanged.

## Reference pages

| Page | Topic |
|---|---|
| [makefile_targets.md](makefile_targets.md) | Every `conductor.mk` target with its help text (generated — regenerate, do not hand-edit) |
| [bootstrap.md](bootstrap.md) | Scaffolding a host project with `python -m conductor bootstrap` |

## Top-level index

See the [project README](../README.md) for how this package fits into a host project;
`AGENTS.md` at the repository root is the working contract these pages elaborate on.
