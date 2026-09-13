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

## Known host issues fixed

SessionEnd/SubagentStop/Stop no longer emit `hookSpecificOutput` (Claude
Code 2.1.268 rejected it); the hook doctor no longer calls a compiled
`forge` binary "dead" for lacking a shebang; a mutation receipt's filename
now always carries the receipt's own `generated_at`, not a second clock
read.
