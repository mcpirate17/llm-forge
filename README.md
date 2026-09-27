# llm-forge

A development environment for building reliable LLM systems: agent coordination,
context and token efficiency, code quality, auditing, testing, and automated
governance.

The Python package is `conductor` (distribution `conductor-tooling`), backed by three
Rust crates under `native/`:

- `src/conductor/` — mutation campaigns and receipts, candidate review and the gate,
  agent-to-agent messaging, knowledge and memory retrieval, session hygiene, worktree
  and sandbox management.
- `src/tooling/hooks/` — the generic agent hooks (Claude, Codex, dispatch adapters) a
  host project wires into its own `.claude/hooks/` or `.agent_hooks/`.
- `native/conductor-native/` — shared Rust algorithms and the optional
  `conductor_native` PyO3 extension behind `conductor/_native.py`.
- `native/slop-core/` — the slop scanner.
- `native/forge/` — the native hook runtime, installer, routing, mailbox, and coordination CLI.

## Standalone Rust build

The Forge CLI builds without a Python interpreter or Python development libraries:

```sh
cargo build --release --locked --manifest-path native/forge/Cargo.toml
native/forge/target/release/forge --help
```

Forge uses `conductor-native` with default features disabled, sharing its planning,
receipt and shell parsing algorithms. The default `python` feature and
`extension-module` feature preserve the existing package bindings. Native task and
mailbox commands run directly in Rust; hook events with remaining legacy handlers
still use their configured host dispatcher. See [mailbox operations](docs/messaging.md)
and [resource-aware task execution](docs/coordination.md). For bounded code
relationships and references extracted from a message, see
[native graph context](docs/graph-context.md).

## Install

To use the current checkout, including changes that have not been published:

```sh
cd /path/to/llm-forge
uv venv .venv --python 3.12
uv sync --extra test --extra mutation --extra graph
.venv/bin/python -m pytest src/conductor -q
make forge-build
export PATH="$PWD/.tools/bin:$PATH"
```

`uv sync` installs the checkout's Python package in editable mode; `make forge-build`
builds the same checkout's native executable locally. The provider selection,
explicit native interpreter binding, coordination, and delivery changes described
below require this current source build until a reviewed revision containing them
is published.

For a reproducible **published snapshot**, no release tag is available yet. The
following real commit predates these additions: it does **not** provide the new
`--provider all` or native `--python` behavior. Pin all three packages because
dependency projects' uv source overrides are not the host's source configuration:

```toml
[tool.uv.sources]
conductor-tooling = { git = "https://github.com/mcpirate17/llm-forge", rev = "d2d83d671977e464effcb44f52dcf470089f655f" }
conductor-native = { git = "https://github.com/mcpirate17/llm-forge", rev = "d2d83d671977e464effcb44f52dcf470089f655f", subdirectory = "native/conductor-native" }
slop-core = { git = "https://github.com/mcpirate17/llm-forge", rev = "d2d83d671977e464effcb44f52dcf470089f655f", subdirectory = "native/slop-core" }
```

Declare all three packages in the host's dependencies so these source pins apply.
Select `conductor-tooling[graph,mutation]` for graph retrieval and Python mutation
campaigns; `test` adds the platform's pytest tools. Native packages build from source
and require a Rust toolchain. The `forge` executable is installed separately with
`make forge-build` in this checkout, or `cargo install --git
https://github.com/mcpirate17/llm-forge --rev d2d83d671977e464effcb44f52dcf470089f655f
--locked --root .tools forge`. Add the resulting `.tools/bin` to PATH. Keep all
components pinned to the same reviewed revision when upgrading.

## Bootstrap a host project

Using the current checkout's environment and native build above:

```sh
.venv/bin/python -m conductor.bootstrap /path/to/host-project
# equivalently: .venv/bin/python -m conductor bootstrap /path/to/host-project
# Explicitly provision additional harnesses and report their supported features:
.venv/bin/python -m conductor bootstrap /path/to/host-project --provider all
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

**Bounded tool output.** The dispatcher's `post_tool_quiet` hook bounds what Read, Grep
and every MCP tool (`mcp__*`) return before the model sees it: above
`TOOL_OUTPUT_QUIET_BYTES` (default 16000, `0` disables) a response is rewritten to
head + elision marker + tail — Grep and MCP results spill to the same directory as
`_bash_quiet`'s Bash spills (`$BASH_QUIET_SAVE_DIR`) and the marker names the spill
path, while Read output is never spilled (the file is on disk) and the marker names the
byte where it was cut plus the `Read(offset=..., limit=...)` call that reaches the rest.

## Installing into a host project

For ongoing host use, install the platform in the host project's own environment.
To use the current local source, replace the published-snapshot source entries with:

```toml
[tool.uv.sources]
conductor-tooling = { path = "/path/to/llm-forge", editable = true }
conductor-native = { path = "/path/to/llm-forge/native/conductor-native" }
slop-core = { path = "/path/to/llm-forge/native/slop-core" }
```

With all three dependencies declared as above, run `uv sync` from the host, then
build its matching executable with `cargo install --path
/path/to/llm-forge/native/forge --locked --root /path/to/host-project/.tools`.
Rebuild the native packages after Rust changes with `uv sync --reinstall-package
conductor-native --reinstall-package slop-core`. The Git pins above remain an
alternative for the older published snapshot; advance them together only after the
desired changes have been reviewed and published.

In the current source build, Python and `forge` hooks both retain bootstrap's
`--python` binding, even when the target has no `.venv`.
The native installer accepts the same explicit binding with
`forge hooks install --host /path/to/host-project --python /path/to/venv/bin/python`.
Mutation snapshots override that binding with their isolated interpreter.

Every CLI (`conductor.active_state`, `conductor.session_preamble`, `conductor.gate`,
...) resolves the *host* repository root from the working directory or environment --
never from where the package itself is installed (`__file__` inside `site-packages` is
not the host tree, and every module that once derived a "repo root" that way has been
fixed). Precedence, highest first:

1. `CONDUCTOR_HOST_ROOT`, when set: an absolute, existing path, and it wins over
   everything else, including an explicit `--repo-root` flag -- useful for pinning
   every subprocess a supervising process launches to one host without threading a
   flag through each one.
2. An explicit `--repo-root` (or equivalent flag) the CLI accepts.
3. The nearest `.git` ancestor of the current working directory, falling back to the
   cwd itself.

A host also configures its own layout in `pyproject.toml`'s `[tool.conductor]` table
(`candidate_policy`, `mutation_registry`, `package_root`, `mutation_receipt_root`,
`integration_branch`, `notes_root`, `crate_roster`) so `conductor.project_paths` stops
assuming this repo's own monorepo-shaped defaults. `crate_roster` defaults to
`tooling/native/crates.toml` -- the crate list `candidate_review.cargo_lint_files` and
a host's own CI both read -- and a configured-but-missing roster fails loud naming the
resolved path rather than linting nothing. Hook commands installed by `conductor.bootstrap`
invoke the CLI the same way a human would (`python -m conductor.<module> ...`, or the
installed console script), from the host's working directory -- there is nothing extra
to configure for path resolution beyond the two items above.

## Documentation

[`docs/`](docs/README.md) has one page per platform law (governance claims, the landing
gate, risk/approval tiers, mutation evidence, context budget, CI coverage), written for a
host project adopting this platform, plus two reference pages:
[`docs/makefile_targets.md`](docs/makefile_targets.md) (every `conductor.mk` target,
generated from its help annotations) and [`docs/bootstrap.md`](docs/bootstrap.md)
(scaffolding a host project). Operational guides cover
[messaging and delivery](docs/messaging.md),
[task coordination and status](docs/coordination.md), and
[guardrail audits](docs/guardrail_audit.md). `AGENTS.md` is the working contract
for changes to this repository itself.

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
