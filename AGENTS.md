# AGENTS.md — llm-forge working contract

`conductor` is the tooling this repository ships **and** the tooling it is governed by.
Every rule below applies to work on `src/conductor/`, `src/tooling/` and `native/`.

## Open work

Known defects live in the GitHub issue tracker, and nothing else in this repository
records them. Before starting, read it — `gh issue list` — and say in your PR body which
issue you are closing, or that you found none covering the work. An agent that skips this
re-derives a defect someone already wrote up, or lands a fix beside an open issue that
still describes the broken behaviour.

## Commits

Conventional subject: `<type>(<scope>): <what and why>`. Every commit carries an
`Agent:` trailer naming the session that wrote it, e.g. `Agent: llm-b0`.
Squash-merge rewrites the author of every landed commit to the repository owner's
account, so `git log` cannot say who did the work — the trailer is the only record that
survives. Git hooks do not add it for you. Write it yourself, beside `Co-Authored-By:`.

## Branches

Branches are temporary. A branch exists to carry one piece of work to `main` and is
deleted on merge. Never open a second branch for the same work. Nothing lands on `main`
except through a pull request.

**No worktrees, ever.** Not `git worktree add`, not a leased tree, not a scratch clone
standing next to the checkout. Work in the checkout you have. The only worktrees this
repository knows about are the disposable snapshots `mutation_engine_generated` creates
and destroys on its own.

## Mutation testing

**Mutate only the files you changed and the tests that exercise them. Nothing else.**
A three-file change is three campaigns, never a repo-wide sweep. `make mutation-plan`
and `make mutation-generate` scope themselves to `git diff <base>...HEAD` plus the dirty
working tree. `--all-files` is a maintenance inventory and does not authorize the runner
to mutate unchanged files.

Mutation evidence is produced **only** by the automatic engines — fest (Python),
cargo-mutants (Rust), Mull (C/C++) — in disposable snapshots. Hand-authored mutants,
patches, manifests, survivor baselines and receipt hashes are forbidden. Never edit a
generated manifest or a receipt: if it is wrong, regenerate it. The three commands are:

```
make mutation-plan
make mutation-generate
make mutation-engine-run MUTATION_CAMPAIGN=campaigns/<id>.json
```

Every new or behavior-changing test needs a current registered **PASS** receipt before
landing. `RATCHET_HELD`, survivors, timeouts, baseline failures and hash drift are not
PASS evidence. Verify with `make mutation-evidence`; `make mutation-coverage` reports the
inventory without executing anything.

Engine child processes run with isolated bytecode caches (`conductor.bytecode_isolation`):
a same-size edit within one mtime second would otherwise execute stale `__pycache__`
bytes, which is exactly what applying a mutant is. Nothing to configure — see
docs/mutation.md's "Bytecode isolation" for why and where.

## Repository data

`candidate_policy.toml` and `campaigns/registry.json` are named by `[tool.conductor]` in
`pyproject.toml` and resolved through `conductor.project_paths`. Do not hardcode either
path in code, in a Makefile recipe, or in a CI job.

Anything a shipped module imports goes in `pyproject.toml`, never only in a CI job.

## Code

`correct > minimal > fast`. No file over 1250 lines, no function over 100 lines.
`uv`, never raw `pip`. Fail loud — no silent fallbacks, no swallowed exceptions.
