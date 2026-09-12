# llm-forge

A development environment for building reliable LLM systems: agent coordination,
context and token efficiency, code quality, auditing, testing, and automated
governance.

The Python package is `conductor` (distribution `conductor-tooling`), backed by two
Rust crates under `native/`:

- `src/conductor/` — mutation campaigns and receipts, candidate review and the gate,
  agent-to-agent messaging, knowledge and memory retrieval, session hygiene, worktree
  and sandbox management.
- `src/tooling/hooks/` — the generic agent hooks (Claude, Codex, dispatch adapters) a
  host project wires into its own `.claude/hooks/` or `.agent_hooks/`.
- `native/conductor-native/` — the `conductor_native` PyO3 extension behind
  `conductor/_native.py`.
- `native/slop-core/` — the slop scanner.

## Install

```sh
uv venv .venv --python 3.12
uv sync --extra test
.venv/bin/python -m pytest src/conductor -q
```

Every host project pins this repo by tag in its own `pyproject.toml`:

```toml
[tool.uv.sources]
conductor-tooling = { git = "https://github.com/mcpirate17/llm-forge", tag = "v0.1.0" }
```

## Bootstrap a host project

Once `conductor-tooling` is installed into a host project's venv, one command scaffolds
everything that project needs to adopt the platform:

```sh
python -m conductor.bootstrap /path/to/host-project
# equivalently: python -m conductor bootstrap /path/to/host-project
```

It writes, idempotently and fail-loud (`--dry-run` to preview, `--check` for CI drift
detection): the dispatcher `hooks` block merged into `.claude/settings.json`, a
`.claude/hooks/dispatch.py` launcher pinned to the interpreter this package is installed
into (no dependency on an in-tree `tooling/` checkout), the code-review-graph server in
`.mcp.json`, a starter `conductor/candidate_policy.toml` and mutation registry, a
`.github/workflows/weekly-audit.yml` running this package's own audits, and a
`.claude/hooks/project/env.sh` stub for repo-specific hook defaults. A settings or MCP
key the host already has is kept; one wired differently is a conflict, refused with a
diff unless `--force`. Project-owned files (policy, preauthorizations, registry,
workflow, env.sh) are created once and never rewritten. Separately, it warns (never
writes) if the host's own `pyproject.toml` is missing the `[tool.conductor]` keys
(`candidate_policy`, `mutation_registry`, `package_root`) that `conductor.project_paths`
needs to find the host's own layout instead of assuming this repo's.

`conductor.bootstrap` is an alias onto `conductor init` (`conductor/project_init.py`),
not a separate implementation — see that module for the full contract.

## Documentation

[`docs/`](docs/README.md) has one page per platform law (governance claims, the landing
gate, risk/approval tiers, mutation evidence, context budget, CI coverage), written for a
host project adopting this platform. `AGENTS.md` is the working contract for changes to
this repository itself.

## Baselines

`make candidate-review` (profile `full`) checks staged Python against four
measurements recorded under `src/conductor/*_baseline.json` and
`conductor/radon_complexity_baseline.json`: jscpd duplication, PMD-CPD
duplication, radon complexity, and vulture dead-code findings. These are
*measurements*, never hand-edited — regenerate them with:

```sh
make baselines            # all four
make baseline-jscpd       # needs jscpd on PATH
make baseline-pmd         # needs pmd on PATH -- see below
make baseline-complexity
make baseline-vulture
```

`jscpd` and `vulture`/`radon` come from `npm install --global jscpd@4.2.1`
(matching `[tools.jscpd]` in `candidate_policy.toml`) and `uv sync --extra
test` respectively.

### Installing PMD locally

PMD has no packaged distribution, so this repo pins the release zip instead
of a system package. Do not `apt install pmd` — install the exact pinned
version so your baseline matches what CI checks against:

```sh
mkdir -p .tools
curl -sSL -o .tools/pmd.zip \
  https://github.com/pmd/pmd/releases/download/pmd_releases/7.27.0/pmd-dist-7.27.0-bin.zip
unzip -q .tools/pmd.zip -d .tools/
rm .tools/pmd.zip
export PATH="$PWD/.tools/pmd-bin-7.27.0/bin:$PATH"
pmd --version   # PMD 7.27.0
```

`.tools/` is gitignored. `[tools.pmd]` in `candidate_policy.toml` pins the
same `7.27.0` version; CI downloads it the same way (see
`.github/workflows/ci.yml`) rather than generating the PMD baseline itself --
baselines are measurements a human regenerates deliberately after a real
refactor, not something CI writes.

### Staleness

`candidate_policy.toml`'s `baseline_expires` gates all four baselines at
once: the whole review refuses once that date lapses, so a baseline can't
silently rot forever unnoticed. Within that window, CI's "duplication,
complexity and dead-code baselines" step re-runs the same jscpd/PMD-CPD/
complexity/vulture checks `candidate-review` uses against every PR's changed
Python files, so drift between the tree and a committed baseline (a new
duplicate pair, a worse complexity block, a new dead-code finding) fails CI
red rather than only surfacing when someone happens to run `make
candidate-review` locally. There is no full-tree hash-based staleness check
for any of the four baselines today (jscpd/PMD-CPD tolerate baseline entries
that no longer exist in the tree without complaint, and vulture's
`generated_from_tree` field is format-checked but never compared against the
current tree) -- `baseline_expires` plus the CI diff-check above are the
mechanisms this repo has.

## History

This repository was split out of the `LLM` monorepo on 2026-09-12 with
`git filter-repo`, keeping the commit history of `conductor/` and `tooling/`. The
monorepo's project data (its mutation campaigns, candidate policy, preauthorizations
and baselines) stayed behind; this repo carries its own dogfood campaigns and policy.
