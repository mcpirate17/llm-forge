# Native console-launcher relocation

`forge worktree-runtime SOURCE_VENV DESTINATION_VENV [--json]` repairs console-script
shebangs in two existing virtualenvs. It replaces the retired
`python -m conductor.worktree_runtime SOURCE_VENV DESTINATION_VENV` interface.
It does not create an environment or worktree, clone files, launch an interpreter,
or install packages. Both positional operands are required; there is no inferred
source, destination or interpreter.

```sh
forge worktree-runtime /path/to/source/.venv /path/to/existing/guest/.venv
```

The command inventories the source `bin` directory and accepts interpreter names
matching `python`, `python3`, `python3.13`, and the original Python utility's numeric
name pattern. A launcher must exist as a regular file at the same name in both
source and destination. Only a complete first line exactly naming one of those
source interpreters is rewritten. Interpreter arguments, different paths, binaries,
script symlinks, directories, special entries and missing destination entries are
left unchanged. Repeating the command reports zero changes.

A matching destination is replaced with a new inode. Its permission bits are
preserved, and an originally hardlinked source retains its bytes and inode. A CRLF
shebang becomes LF, matching legacy behavior; all bytes after that first newline
are preserved exactly, including CRLF or NUL bytes. A shebang without a terminating
newline is left unchanged. The destination interpreter path is not executed or
validated as a runnable runtime by this utility.

## Safety and bounds

Source and destination roots and `bin` directories must be separate existing
inodes. Directory components are opened without following symlinks, and script
symlinks are never opened for reading or publication. The command takes
nonblocking cooperative flocks on the two pinned `bin` directories and reports a
busy operation rather than waiting or creating a persistent lock file.

Interpreter discovery has one narrow read-only exception: source Python symlink
names may be followed for metadata only to confirm a regular-file target. The OS
symlink-depth limit applies. The link identity, literal target and resolved target
identity are rechecked. Dangling links and special/nonregular targets do not count
as interpreters. The discovery step never opens or reads an interpreter symlink referent, and
never executes it or writes through its link. As in the legacy loop, ordinary
regular-file candidates can receive a bounded first-line probe even when their
name is `python3`; valid regular wrapper scripts are not skipped by basename.

Limits are 100,000 visited source entries, 255-byte filenames, 4,096-byte directory
paths, 256 path components, a 4,097-byte first-line probe, 64 MiB per matching
input/output script, and 2 GiB of cumulative matching input plus output bytes.
Foreign or binary large files are skipped after the bounded first-line probe.
Each script is processed separately, so memory use does not grow with all script
bodies. The result list and name inventory remain bounded by entry and path limits.

Publication uses an exclusively created staging inode and an atomic exchange in
the held destination directory. The displaced original remains available until
identity/content checks and directory synchronization succeed. Detected foreign
replacements or edits cause conditional retention rather than ordinary temporary
cleanup. Failed postchecks retain the displaced slot and report its last known
path and held `bin` inode. A final directory-sync failure after successful cleanup
can report an error after the old slot is already removed. An interrupted setup
may retain a named staging file; inspect the reported path before removing any
artifact.

This is a sequence of per-file replacements, not a batch transaction. An error
reports completed earlier paths and any uncertain current publication. SIGINT and
SIGTERM are polled through the native interrupt supervisor. Cancellation is
suppressed across an exchange's validation and cleanup, then the original signal
is propagated before a success report can be emitted.

The selected directories are operator-controlled and all participating writers
must honor their flocks. Linux does not provide an inode-conditional rename or
unlink: arbitrary writers ignoring these locks can race the last check and
publication/cleanup. The command detects and retains displaced data when checks
fail, but does not promise atomic isolation against such writers or a durable
crash-recovery journal. It never rolls foreign bytes back over a changed path.

## Result contract and migration

Text success remains `relocated N Python console script(s)`. Exit 0 indicates the
completed invocation; errors and argument failures return 2. Native signal
termination is propagated as a signal after publication safety cleanup.

JSON adds a versioned native contract: `schema_version: 1`,
`interface: "forge.worktree-runtime"`, `outcome: "relocated"`, `relocated_count`,
and `relocated`. Each relocated entry contains its normalized absolute `path`
(null for non-UTF-8 names) and lossless Unix `path_bytes`. Failures emit one JSON
error envelope when requested and a diagnostic on stderr.

The old Python API, its PyO3 parity target, Cargo registration and two registry
rows are retired together. Native core tests and actual binary tests preserve the
original three contract groups and add bounds, metadata-only interpreter links,
CRLF, cancellation, non-UTF-8 result encoding and publication-race controls.
Existing workspace-runtime model probes are unrelated and remain a separate CLI.
