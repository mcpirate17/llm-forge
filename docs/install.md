# Installing forge into a host's Claude Code settings

One binary, three commands (`docs/roadmap.md` Phase 4's first Rust piece):
`forge hooks install`, `forge hooks status`, `forge hooks uninstall`. They
edit the host project's `.claude/settings.json` — merging, never replacing:
an entry counts as forge's iff one of its hook commands ends with
`forge hook <Event>`; every other entry, every other top-level key, and the
key order are preserved.

Install the binary itself first:

```
cargo install --git https://github.com/mcpirate17/llm-forge --locked forge
```

## Install

```
forge hooks install --host <project-root> --mode warn --standalone
```

- `--mode warn|enforce` (default `warn`) is the `FORGE_MODE` the installed
  commands run with. Start with warn: it reports what routing and the live
  cap check *would* deny, without denying it (`docs/routing.md`, "Warn-only
  mode and standalone install").
- `--standalone` installs only `PreToolUse` (matcher `.*`) and
  `SubagentStop`, each with `FORGE_HOOK_STANDALONE=1` in its command
  prefix: forge answers alone — no Python dispatcher, no native Bash-guard
  branches. Omit it on a host that carries llm-forge's Python tooling: all
  five events are installed with `FORGE_MODE` only, and forge delegates
  what it has no native handler for to the host's dispatcher.
- `--binary PATH` (default: the running `forge`'s absolute path).
- `--dry-run` prints the unified diff and writes nothing.

Before the first write the current settings are copied once to
`.claude/settings.pre-forge.bak.json` — the backup is never overwritten,
however many times install is re-run. Re-running install is idempotent: it
rewrites exactly forge's own entry per event in place (so a mode or binary
change is one command) and collapses duplicates a hand edit may have left.

## The warn → enforce flip (scheduled ~2026-09-20)

```
forge hooks install --host <project-root> --mode enforce --standalone
```

Same command, different mode: the entries are rewritten in place, nothing
else in the file moves.

## Status and rollback

```
forge hooks status --host <project-root>
forge hooks uninstall --host <project-root>
```

`status` prints one line per event — installed or not, mode, standalone,
binary, whether the binary exists, and whether its `forge --version`
matches the running one (`forge --version` prints the git rev the binary
was built from). It exits 1 when an installed entry points at a missing
binary: that hook would silently do nothing. `uninstall` removes exactly
the entries install recognises (all five events, whatever the install
flags were), prints what it removed, and deliberately leaves the backup in
place — rollback is either `uninstall` or restoring
`.claude/settings.pre-forge.bak.json` by hand.

## Take over from the Python dispatcher

A host that runs llm-forge's Python tooling alongside forge (i.e. installed
without `--standalone`, or hand-wired with both) pays for `dispatch.py
<Event>` (~40-50 ms, interpreter start included) *and* forge (~1-3 ms) on
every hook event, even for events forge already answers on its own.
`--takeover` removes that duplication for events where nothing is lost:

```
forge hooks install --host <project-root> --mode warn --takeover
```

`--takeover` implies `--standalone`. For each event, `forge hooks
install` asks `native/forge/src/takeover.rs::coverage(event, tool_matcher)`
whether every Python hook that event's host matcher would run has a native
twin:

- **Full** (`PostToolUse` today — see the coverage table below): the
  host's Python dispatcher entry (`.../dispatch.py <Event>` or
  `python -m tooling.hooks.dispatch <Event>`) is removed and recorded
  **verbatim** in `.claude/settings.forge-takeover.json` (created once;
  re-running `--takeover` merges into it and never blind-overwrites an
  entry it already recorded), and a forge entry for that event is ensured.
- **Partial** (`PreToolUse`, `SessionStart`, `SessionEnd` today): the
  Python entry is left completely untouched and reported as `kept python:
  <event> (missing: …)` — naming the Python hook names that have no
  native twin yet, or, for `PreToolUse`+`Bash`, naming why: forge's
  standalone entrypoint never invokes the Bash guard logic at all (see
  the coverage table's note below).

`--dry-run` prints what would change without writing anything.
`forge hooks status` gains a `python: present|taken-over|n/a` column so a
re-run (or a different operator) can see at a glance which events still
run Python. `forge hooks uninstall` restores every recorded Python entry
verbatim and deletes `.claude/settings.forge-takeover.json` — the same
rollback command works whether or not `--takeover` was ever used.

## Session start in Rust

`forge session preamble` replaces the Python SessionStart inject pair —
`conductor.active_state update` (background) plus `conductor
.session_preamble hook` — with one process that refreshes
`conductor/active_state.json` itself and prints the same hook-payload
JSON; `forge session state [--dump]` is the state half alone
(`--dump` prints the JSON instead of writing it). The generic
`session-start.sh` runs the binary whenever `FORGE_BIN` (env) or
`command -v forge` resolves it — the A2A name still comes from the
identity the script resolved, the summary from `A2A_SUMMARY`. Two ways
to tell which path ran: the Python path logs exactly one stderr line,
`[session-start] forge not on PATH; python preamble (slower)`; and
`forge session preamble --host <root> --a2a-name <id> --text` prints the
inject body directly, so a manual run shows what the binary would inject.
Set `FORGE_BIN=` (empty) to force the Python stages — the same convention
as `FORGE_NATIVE_HOOKS`. The integration line the landed-worktrees count
judges against resolves **offline** (configured branch, bound symref, or
the one conventional `origin/{master,main}` ref): session start never
opens a network connection — `ls-remote` is the reaper's, not the hook's.
To pin the line explicitly, set `CONDUCTOR_INTEGRATION_BRANCH` or bind the
symref once with `git remote set-head origin -a`.

## Verify the install

```
forge doctor --host <project-root> [--json]
```

One line per check, `PASS|FAIL|SKIP <check>: <detail>`, six checks:
`settings` (forge's entries in the host's `.claude/settings.json`),
`binary` (each installed entry's binary exists, is executable, and its
`forge --version` rev matches the running one), `python` (the interpreter
forge would delegate to imports `conductor` and `tooling.hooks.dispatch`
from the host; SKIP on a `--standalone` install, which never delegates),
`ledger` (the ledger root is writable — a probe file is created and
deleted under `live/`), `policy` (the embedded `routing_policy.toml`
parses, with its class count), and `hook-roundtrip` (this binary re-run
exactly as the standalone install wires it, a synthetic `Bash` payload on
stdin; the probe writes nothing). Sample:

```
PASS settings: 2 entries over [PreToolUse, SubagentStop], mode=warn, standalone=true
PASS binary: 1 binary checked, versions match 0.1.0 (git 890e5e5)
SKIP python: every installed entry is standalone; forge never delegates to Python
PASS ledger: /mnt/data/llm/ledger is writable (probe under live/ created and deleted)
PASS policy: embedded routing_policy.toml parses: policy_version 2026-09-13.1, 4 classes
PASS hook-roundtrip: exit 0, stdout empty, 1 ms
```

`--json` prints the same as one `{"checks": [{"name", "status",
"detail"}], "ok"}` object. Exit 1 means: fix whatever the one `FAIL` line
names — that line is the broken piece, not a suggestion to re-run install
blind (`forge hooks status` remains the narrower settings-only check).

## Known host issues fixed

SessionEnd/SubagentStop/Stop no longer emit `hookSpecificOutput` (Claude
Code 2.1.268 rejected it); the hook doctor no longer calls a compiled
`forge` binary "dead" for lacking a shebang; a mutation receipt's filename
now always carries the receipt's own `generated_at`, not a second clock
read.
