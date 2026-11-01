# Installing native Forge

From a checked-out source tree, build the native binary with the pinned toolchain:

```sh
cargo +1.98.0 install --path native/forge --locked
```

The default executable dependency graph contains no Python interpreter, PyO3 or
Node runtime. `make build`, `make install` and `make test` also use native Cargo
artifacts. Make installs into this checkout's ignored `target/install` by default;
set `INSTALL_ROOT` to select another prefix. Shared virtual environments and
installed user binaries are untouched by that default. The root Python package
and remaining maintenance recipes are still being retired; their presence is
not evidence of a complete native
migration. This command installs the executable, not a provider client or model.

## Host configuration

`forge project --host /path/to/project init` plans a native `forge.toml`, copying
existing host settings when present. Add `--apply` to publish it with a rollback
snapshot. Invalid declared paths or session policy fail before publication;
existing native manifests keep their bytes and permissions. See
[native project configuration](native-project-config.md).

For MCP, start an explicitly configured native harness and run
`forge mcp --host /path/to/project bind --socket /returned/state/socket`.
Apply after reviewing the plan. The emitted native command checks the host
binding at startup; setup does not start or extend the service. See
[native MCP setup](native-mcp-setup.md).

## Provider configuration

Use the native provider installer for a new installation or migration:

```sh
forge providers --host /path/to/project --provider claude install --mode enforce
forge providers --host /path/to/project --provider claude install --mode enforce --apply
forge providers --host /path/to/project --provider claude check --mode enforce
```

The first command prints a read-only plan. Add `--apply` to publish it. Select
`codex`, `qwen` or `grok` explicitly for another provider. Claude and Codex receive
native guard and lifecycle hooks; Qwen and Grok receive bounded mailbox startup
only. A native installation does not create an A2A registration or infer an
identity from the provider name.

`--replace-legacy` explicitly replaces recognized Forge Python hook entries.
Unrelated settings and commands are retained. Each changed operation records an
exact rollback snapshot; `rollback --apply` refuses to overwrite later edits.
Configuration parents and files have ownership and permission requirements.
See [native-providers.md](native-providers.md) for command ownership, path safety,
rollback, client support boundaries and once-per-session behavior.

All matching native guards run in `forge hook`; no event delegates to Python.
`FORGE_NATIVE_HOOKS` cannot disable guards or select a fallback. See
[native-lifecycle.md](native-lifecycle.md) for startup, native journal, host
extensions, binary freshness and provider output projection.

The old `conductor init`/`bootstrap` command, checkout Python dispatcher and
shipped Python/shell hook bodies have been removed. Use the native project,
provider and MCP commands above when upgrading a host. Existing external
settings are changed only by an explicit install/apply operation; Forge does
not silently rewrite other checkouts. `crg-sync` no longer installs extensions
into an interpreter. `make crg-check MCP_SOCKET=/returned/state/socket` checks
the native binding instead.

## Existing Claude hook commands

The earlier Claude-specific interface remains available:

```sh
forge hooks install --host /path/to/project --mode warn --standalone --dry-run
forge hooks status --host /path/to/project
forge hooks uninstall --host /path/to/project --dry-run
```

`--standalone` installs only PreToolUse and SubagentStop. Without it, all five
native events are installed. Unlike `forge providers`, this older interface
writes by default; use `--dry-run` for planning. It keeps its first-install backup
at `.claude/settings.pre-forge.bak.json`. Status recognizes explicit native host
and protocol flags and uses a bounded binary version probe.

The older `--takeover` mechanism retains its recorded rollback contract and
limited event selection. Its `python: present|taken-over|narrowed` status describes
external configuration ownership, not an interpreter dependency of the native
hook. Use `forge providers install --replace-legacy` for a complete native
provider configuration. Uninstalling a previous takeover may restore its recorded
Python entries; review the plan before changing an older host installation.

## Verify native hooks

```sh
forge doctor --host /path/to/project --json
```

The Claude doctor checks settings, the binary, ledger writability, embedded
routing policy and a bounded native hook roundtrip. It creates and removes a
small ledger probe. There is no Python import check. For other providers, use
`forge providers ... check` with the same mode, identity and binary options used
for installation. Configuration checks do not prove that a particular installed
client implements every configured event.

## Prose search of notes

```sh
forge notes index --host /path/to/repo --vault /path/to/vault
forge notes search --host /path/to/repo "binding wall semiring" --limit 10 --json
```

The native index combines host `research/notes` and `tasks` Markdown with the
selected vault's research, dashboard and runbook trees. Search queries the
existing FTS5 index. The default database is `research/notes.db`; `--db` selects
another path. These are prose tools. Native structural and semantic code search
are described in [native-code-search.md](native-code-search.md).
