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
re-deriving"). Unlike `candidate_policy`, `mutation_registry`, `package_root` and
`mutation_receipt_root` — which all resolve through `conductor.project_paths`, honouring an
environment variable and then a `[tool.conductor]` key in `pyproject.toml` before falling
back to a default — **`kb_retrieve`'s notes directory has no equivalent knob**:

```python
# src/conductor/kb_retrieve.py
ROOT: Final[Path] = Path(__file__).resolve().parents[1]
NOTES_DIR: Final[Path] = ROOT / "research" / "notes"
```

`ROOT` resolves to this package's `src/` directory (one level above `src/conductor/`), so
`NOTES_DIR` currently means `<repo>/src/research/notes/` — there is no environment variable
and no `[tool.conductor]` key that overrides it, and the `index`/`query` CLI subcommands
take no `--notes-dir` flag either. `conductor.memory_index` inherits the same `ROOT`
pattern for its own catalog file. A host project that wants `kb_retrieve query "..."` to
search its **own** knowledge cards today has exactly one option: place `kb_*.md` cards at
that literal path.

**This repository's own `docs/` cannot be indexed by `kb_retrieve` as it stands**, for two
reasons: the pages here are named `KB-<ID>.md` (uppercase, hyphenated) rather than the
`kb_*.md` glob `load_cards()` requires, and `docs/` is not `src/research/notes/` regardless
of naming. Renaming the pages to fit the glob would not be enough on its own.

**Debt, not fixed here:** adding a real notes-root knob is a code change to
`conductor.kb_retrieve` and `conductor.project_paths` (a new `NOTES_ROOT_KEY` /
`CONDUCTOR_NOTES_ROOT` env var following the existing `project_paths.py` precedence
pattern, a `notes_root()` resolver, and `load_cards()`/`memory_index`'s catalog resolution
updated to use it), plus the tests and mutation evidence that change requires under
`KB-MUT-02`. That is out of scope for a documentation-only change. Until it lands:

* A host project that wants its own cards indexed should place them under
  `<repo>/src/research/notes/kb_*.md` (matching the current hardcoded resolution) and name
  each file `kb_<topic>.md`, lowercase, matching `load_cards()`'s glob.
* This repository's own dogfood indexing of `docs/` is blocked on the same debt — it is not
  wired here for the same reason a host project cannot repoint it today.

## Top-level index

See the [project README](../README.md) for how this package fits into a host project;
`AGENTS.md` at the repository root is the working contract these pages elaborate on.
