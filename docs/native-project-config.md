# Native project configuration

Native path resolution, workspace integration-branch discovery and SessionStart
policy share one Rust manifest reader. A host can configure them without a
Python package:

```toml
# forge.toml at the host repository root
schema_version = 1

[conductor]
integration_branch = "main"
candidate_policy = "candidate_policy.toml"
mutation_registry = "campaigns/registry.json"
mutation_receipt_root = "campaigns/receipts"
notes_root = "docs"
native_root = "native"

[conductor.session]
preamble = ["Use the host's documented build and test commands."]
standing_mandates = ["Preserve unrelated working-tree changes."]
```

A present `forge.toml` is authoritative. Its conductor table is not merged with
`pyproject.toml`; omitted settings use the resolver's defaults. A malformed,
unreadable or unsupported native manifest is an error. Only when `forge.toml`
is absent does the reader accept `[tool.conductor]` from `pyproject.toml` as
legacy host data. Neither format starts an interpreter. This fallback lets
native Forge operate in existing Python projects; it is not a runtime dependency
on their packaging tools.

Both inputs must be regular files, strict UTF-8 TOML and at most 64 KiB. The
reader opens with `O_NOFOLLOW | O_NONBLOCK`, checks the opened descriptor and caps
the read even if the file grows. Final symlinks, directories and FIFOs are
rejected. Parent directories and concurrent same-UID writers remain part of the
operator's trust boundary. Duplicate TOML keys are rejected by the parser.

Native manifests require integer `schema_version = 1`. The optional conductor
section must be a table. The shared resolver retains these path settings and
per-call environment overrides:

| Key | Environment override | Default |
| --- | --- | --- |
| `candidate_policy` | `CONDUCTOR_CANDIDATE_POLICY` | `conductor/candidate_policy.toml` |
| `mutation_registry` | `CONDUCTOR_MUTATION_REGISTRY` | `conductor/mutation_campaigns/registry.json` |
| `package_root` | `CONDUCTOR_PACKAGE_ROOT` | `conductor` |
| `mutation_receipt_root` | `CONDUCTOR_MUTATION_RECEIPT_ROOT` | `research/reports/mutation_testing` |
| `notes_root` | `CONDUCTOR_NOTES_ROOT` | `research/notes` |
| `notes_db` | `CONDUCTOR_NOTES_DB` | `research/notes.db` |
| `guardrail_allowlist` | `CONDUCTOR_GUARDRAIL_ALLOWLIST` | `conductor/guardrail_allowlist.json` |
| `memory_sources` | `CONDUCTOR_MEMORY_SOURCES` | `conductor/memory_sources.toml` |
| `crate_roster` | `CONDUCTOR_CRATE_ROSTER` | `tooling/native/crates.toml` |
| `native_root` | `CONDUCTOR_NATIVE_ROOT` | `tooling/native` |
| `radon_complexity_baseline` | `CONDUCTOR_RADON_BASELINE` | `conductor/radon_complexity_baseline.json` |

Paths must be nonempty and repository-relative, with no parent traversal. The
resolver normalizes separators and redundant `.` components. These lexical
checks do not themselves grant filesystem capabilities; tool handlers enforce
those separately. `CONDUCTOR_INTEGRATION_BRANCH` overrides `integration_branch`;
the default is `main`. `retired_integration_branches` and `worktree_patterns`
accept arrays of nonempty strings.

Session policy is optional. When present, its table must contain exactly
`preamble` and `standing_mandates`, both arrays of nonempty strings. This text
supplies context and does not grant approval or execution authority. The native
SessionStart dispatcher reads the same selected manifest as path resolution.

## Initialize or migrate a host

```sh
forge project --host /absolute/project init
forge project --host /absolute/project init --apply
forge project --host /absolute/project show
forge project --host /absolute/project rollback --apply
```

`init` defaults to a read-only plan. If `forge.toml` is absent, it copies only
the existing `[tool.conductor]` table into the native format, or writes an empty
table when neither manifest exists. Python packaging metadata is not copied.
Declared paths, branch/list settings and session policy are validated before
publication, even when an environment override would mask an invalid value.
Unknown conductor keys remain intact for other native consumers. Existing
native manifests keep their exact bytes, comments and permissions after validation.

Apply requires an owned host directory and regular, singly linked input files
that are not writable by other users or groups. New manifests are mode 0600.
The permanent `.forge-project.lock` serializes cooperating writers; the command
rechecks the source and target snapshots before publication. A private
`forge.toml.forge-native.bak.json` records the exact before/after bytes and modes.
Backup and target are individually fsynced and renamed, followed by directory
fsync. This is durable per-file publication, not a two-file transaction. A crash
after the backup write can leave the old target; rollback recognizes that state.

Rollback defaults to a plan and refuses later user edits or a changed backup.
It restores the previous manifest or its absence. Repeating init/rollback is
idempotent. Same-UID writers outside the cooperative lock remain trusted.
`show` reports the selected manifest, key inventory, resolved paths and branch;
it does not print session text or arbitrary configuration values.

These commands do not create governance policy files or start services. Install
client hooks with the separate [native provider command](native-providers.md),
then bind an explicitly started harness using [native MCP setup](native-mcp-setup.md).
Remaining interpreted package adapters are tracked in
[ARCHITECTURE.md](../ARCHITECTURE.md).
