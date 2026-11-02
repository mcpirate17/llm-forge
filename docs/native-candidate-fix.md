# Explicit native candidate fixes

`forge candidate fix` fixes explicitly selected current-worktree Python files
using an operator-granted, SHA-pinned native Ruff executable. Native Rust owns
admission, supervision, configuration binding, output capture and publication.
There is no uv/Python coordinator or automatic tool download. The former Python
candidate-review `fix` subcommand is removed; other Python candidate-review
commands and consumers remain.

## Provision the tool and execution capabilities

The supported stdin/output contract is Ruff **0.16.5**. Supply that native ELF
through the operator's provisioning process, place it outside the source and
scratch roots, and record its SHA-256. A private copy can be provisioned without
changing a shared installed executable:

```sh
install -m 0755 /operator/supplied/ruff-0.16.5 /operator/tools/ruff
sha256sum /operator/tools/ruff
```

The destination parent must already exist, be owned and not group/world
writable. Native admission also checks executable/file ownership, permissions,
canonical paths and regular-file identity. A version string establishes supported
behavior; it is not a publisher signature. The external grants and supplied SHA
identify the executable the operator authorized.

Create an owned grants JSON outside the repository. This is the existing native
execution grant schema. An illustrative Linux configuration is:

```json
{
  "version": 1,
  "source_root": "/owned/repo",
  "read_only": ["/owned/repo", "/operator/tools", "/usr", "/etc/ld.so.cache"],
  "programs": {"ruff": "/operator/tools/ruff"},
  "environment": {},
  "scratch_parent": "/operator/fix-scratch",
  "cgroup_parent": "/sys/fs/cgroup/operator-delegation",
  "resources": {"cpu_millis": 1000, "memory_mib": 512, "processes": 16},
  "max_wall_seconds": 60,
  "max_cpu_seconds": 30,
  "max_address_space_mib": 1024
}
```

Use actual canonical paths and runtime-library read capabilities for the host.
The common Git directory also needs a read grant when outside the source root.
Provision the owned scratch parent outside source/common-Git metadata and an
owned delegated cgroup with the required controllers. Forge does not establish
that delegation or change parent limits. Linux Landlock ABI 6, seccomp and the
native supervisor prerequisites are required for execution; unavailable support
fails visibly. See [native execution](native-execution.md). The grants file must
be owned, regular, singly linked and not group/world writable. No Python alias
is needed for this command.

## Plan and execute

The default only admits and binds inputs; it does not invoke Ruff, including its
version command, and does not write sources:

```sh
forge candidate fix --repo /owned/repo --grants /operator/fix-grants.json \
  --ruff-sha256 <64-hex-sha256> -- src/example.py package/types.pyi
```

Add `--execute` to run version admission, then Ruff check/fix for every selected
file, then formatting for every selected file, and publish the successful batch:

```sh
forge candidate fix --repo /owned/repo --grants /operator/fix-grants.json \
  --ruff-sha256 <64-hex-sha256> --execute -- src/example.py package/types.pyi
```

Use literal repository-relative `.py`/`.pyi` files after `--`. Directories,
globs, option-looking paths, duplicates, traversal, symlink aliases, hardlinks,
unowned/read-only files and unsupported encodings are refused. The input must be
UTF-8 without BOM/NUL; non-UTF-8 PEP263 coding cookies are refused even when all
current bytes are ASCII. Filenames remain argv data. There is no free-form Ruff
argument escape hatch that can widen the selected write scope.

For Make or filenames containing whitespace, write a JSON string array to an
absolute canonical owned file:

```json
["src/example.py", "package/types.pyi", "scripts/name with spaces.py"]
```

```sh
forge candidate fix --repo /owned/repo --grants /operator/fix-grants.json \
  --ruff-sha256 <64-hex-sha256> --paths-from /operator/fix-paths.json --execute
make governance-fix FIX_PATHS_FILE=/operator/fix-paths.json \
  FIX_GRANTS=/operator/fix-grants.json FIX_RUFF_SHA256=<64-hex-sha256>
```

`--paths-from` and positional files are mutually exclusive. The Make target
executes explicitly and uses `CONDUCTOR_HOST_ROOT`/`FORGE`. The former
`FIX_PATHS='path ...'` variable is replaced by `FIX_PATHS_FILE`; filenames in the
JSON file are never shell-expanded.

## Configuration and bytes

Ruff receives each original absolute filename through `--stdin-filename` and the
original repository cwd. Native admission binds ancestor `.ruff.toml`,
`ruff.toml`, `pyproject.toml`, their absences and bounded literal `extend` chains.
It rechecks configuration/source/tool inputs around execution and publication.
Nested project configuration and force-excluded input behavior are preserved;
Ruff's excluded stdin path parrots the source, producing no spurious deletion.

External ancestor/extended configuration needs both a read capability and an
explicit pin, for example:

```sh
forge candidate fix --repo /owned/repo --grants /operator/fix-grants.json \
  --ruff-sha256 <ruff-sha256> \
  --config-pin /operator/shared-ruff.toml=<config-sha256> \
  --execute -- src/example.py
```

Implicit ambient user configuration is disabled: HOME and XDG configuration
paths are private scratch paths. No-project-config projects use the pinned Ruff
version's defaults without an extra switch. To use a shared configuration, make
it an explicit project `extend` and pin/grant the external file. Symlink extension
paths, environment/tilde path interpolation and selected-extension remaps to
non-Python kinds are refused. Unsupported Ruff version/schema/configuration
errors are visible; dry admission does not claim Ruff validated its schema.

The child sees source as read-only and emits candidates on stdout. Literal
`check --fix` and `format` stdin commands disable caches and color; diagnostics
use stderr. The supervisor captures exact bounded bytes, retaining CRLF and
empty output rather than diagnostic-tail normalization. Ruff may intentionally
change line endings under its configuration. Empty successful output may remove
all file contents. Invalid UTF-8 output or invalid native Python syntax refuses
publication. Other granted read-only package/runtime inputs remain an operator
stability boundary; the report does not recursively attest them.

## Failure, cancellation and recovery

All tool stages must succeed before any selected source is published. A failed
check, failed format, partial stdout, input drift or output/syntax refusal leaves
the complete selected batch unchanged. Natural child nonzero/signal status is
preserved; timeout returns 124, parent SIGINT/SIGTERM 130/143 after cleanup, and
admission/supervisor errors use the CLI error path (normally 2). Planning success
is not an executed fix, test PASS, review approval or commit authorization.

Publication stages all edits under one cooperating-writer root lock, rechecks
identities, retains original Git blobs under `refs/forge/undo/<blob>`, then
renames/synchronizes files sequentially. This is not filesystem-wide atomic
visibility. A later publication failure or cancellation starts reverse rollback.
Rollback restores only files whose candidate bytes and staged inode identity
still match; it preserves detected concurrent edits/replacements and continues
cleanup for other files. Errors include retained recovery blob IDs and conflicts.
Cancellation is polled around publication and during rollback; cleanup completes
before disposition. An interrupt observed after completed publication can return
an interrupt status with applied edit reports. Writers ignoring the advisory lock
can still race verification and rename; hostile same-user ABA races are outside
the trusted-parent contract.

Successful JSON edit reports include prior/candidate hashes and `git_snapshot`.
Use the existing guarded restore command to inspect recovery before explicitly
applying it, supplying the file's **current** SHA-256 and the retained blob ID:

```sh
forge code --root /owned/repo restore src/example.py \
  --expected-hash <current-file-sha256> --git-snapshot <retained-blob-id>
# Add --apply only after reviewing that recovery plan.
```

One owned scratch directory survives through version/check/format. Safe cleanup
removes only that scratch; `--keep-failed` retains failed-filter scratch and prints
its path. Unsafe cleanup retains automatically. Caller-owned source, runtime,
tool and cgroup parent directories are not deleted. Recovery refs intentionally
remain available; scratch retention is not an automatic source rollback command.

Limits are 1–256 files, 1 MiB/file, 16 MiB original source total, 32 MiB combined
original/candidate bytes, 32 MiB executable, 1 MiB combined stdout/stderr per stage,
64 KiB JSON paths input, 8 MiB configuration bytes, 1024 config candidates,
32-level extend chain and 128 external config pins. The grant's wall budget spans
all tool stages; native process/memory/CPU limits also apply.

## Validation boundary

The integrated native tests include inexpensive admission/pipe/transaction cases
and opt-in real confined execution cases for success, partial failures, signals,
deadline cleanup, source/configuration drift, exclusion and retained scratch.
The real-Ruff fixture requires an explicit native 0.16.5 executable and delegated
cgroups; it privately copies and hashes the tool. All seven admission cases and
ten explicitly provisioned execution cases passed on the integration host,
including actual Ruff configuration/exclusion behavior and child cleanup. This
does not establish clean-host execution or arbitrary Ruff-version compatibility.
The separate [native gate](native-gate-retirement.md) and
[candidate review](native-candidate-review.md) retain their own execution and
evidence contracts.
