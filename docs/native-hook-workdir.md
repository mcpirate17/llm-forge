# Native Bash hook workdir scope

The native claim gate resolves a Bash command's relative writes from
`tool_input.workdir`, then `tool_input.cwd`, then the hook payload's top-level
`cwd`. If none is supplied, it retains the protected project root as the
legacy default. A relative per-tool directory starts from the top-level cwd
(or protected root when absent). Literal `cd` commands update that starting
directory before target classification.

The protected root is a separate input. Each resolved absolute write is checked
against that checkout and any linked checkout sharing its Git common directory.
Writes into other repositories and scratch directories have no claim in the
protected checkout. An absolute protected path, `../` path into the protected
checkout, or path into a linked worktree remains gated even when the shell runs
from another repository. Opaque interpreter writes remain gated because their
actual destination cannot be classified.

The ordered resolver binds each recognized write to the cwd in effect at that
command, including nested `bash -c` scripts, shell heredocs, and `env -C`. It
retains separate results for the same relative filename written after
different `cd` commands. Unresolved relative writes after a cwd change return
an opaque write for the gate to deny. The original raw target extractor remains
available for compatibility.
For a bare relative `cd`, a nonempty `CDPATH` makes the destination uncertain;
the resolver treats a later relative write as opaque. Paths beginning `./` or
`../` bypass `CDPATH` and keep their ordinary relative meaning.

For each literal target, the gate considers the lexical path, the directory
entry reached through its parent, and the destination reached by following the
final symlink. It resolves the longest existing ancestor before appending any
missing directories. This protects symlink removal and writes through a link,
including `install -D` into new nested directories.

This is a static hook preflight for recognized shell writes. It does not run
the command or provide a complete shell sandbox. Uninspected shell scripts such
as `bash script.sh`, dynamic writes inside command substitutions, and sourced
files return an opaque write and require a more explicit command. Read-only
substitutions such as `echo "$(date)"` remain allowed.

The native `absolute_write_targets` API exposes resolution without a protected
root. `repo_write_targets` retains its original two-argument behavior for
existing callers, while `repo_write_targets_from` accepts distinct command cwd
and protected root. The native handler uses the latter for its informational
target list; the native claim gate classifies the absolute targets directly.
Forge 0.8.1 adds the `forge legacy-hook gate-verify-bash` JSON operation for a
future Python compatibility delegation. A caller adopting this operation must
require a binary that supports it; older 0.8.0 binaries do not expose it.

The installed LLM Codex hook currently enters Forge's Python compatibility
gate. Its source and installed runtime need separate delegation and activation
work before this native fix changes that observed hook path. The operation is
not yet called by that Python gate. This change does not modify hook
configuration or installed packages.
