# Native equivalence gate

`forge equivalence gate` probes selected whole Python modules from an explicit Git
commit or captured live inputs using the existing confined native execution owner. Forge orchestration
is Rust; the operator grants the interpreter, external Slop Core guest and host
dependencies needed to execute the Python workload.

```sh
forge equivalence gate --root /path/to/repository \
  --commit FULL_LOWERCASE_40_CHARACTER_COMMIT_OID --module src/model.py \
  --grants /operator/execution-grants.json \
  --guest-path /operator/slop_core.so --guest-sha256 GUEST_SHA256 \
  --json --enforce
```

The example requires real operator-owned files and a delegated cgroup. It does
not create grants or discover an interpreter. Use the installed executable's
`equivalence gate --help` for all options. The command requires Linux and the
same sandbox/resource prerequisites as [candidate review](native-candidate-review.md).

## Source and input contract

Select exactly one of `--commit` and `--live`. A commit is a full lowercase object identity resolving to a commit;
branch names, tags, abbreviations and tree objects are refused. Repeat `--module`
for literal relative regular `.py` blobs. Explicit modules retain argument order;
duplicates are refused. Selection covers each whole module, including
units outside any changed lines. Configuration, waivers and test discovery use
that commit's captured tree. Staged, dirty, untracked and ignored checkout files
are outside the evaluated source scope.

The capture tree and a separate execution workspace are verified against the
committed preimage before launch. After guest cleanup, the retained capture tree
is checked again and explicitly disposed before publication. The guest's own
workspace can change during probing and is disposed by the existing owner.
Verification checks leaf bytes, file modes, literal links, parent directories
and unexpected entries, including empty directories. It refuses gitlinks and
depths over 256. Existing snapshot limits remain 64 MiB per blob, 2 GiB per tree
and bounded path inventory; selected Python inputs have their smaller existing
capture limits.

### Explicit live inputs

```sh
forge equivalence gate --root /path/to/repository \
  --live --input src --input tests --input data --module src/model.py \
  --grants /operator/execution-grants.json \
  --guest-path /operator/slop_core.so --guest-sha256 GUEST_SHA256 \
  --json --enforce
```

Each `--input` names an existing relative regular file or directory. Supply one
to 256 distinct, nonoverlapping roots; `.` explicitly selects the repository
root. Capture includes every tracked HEAD/index path with its current working
bytes, every file under the declared roots, and root `pyproject.toml` when
present. Staged contents never override working bytes and deleted files are not
restored from Git. Declared roots include ignored, untracked, binary and
extensionless files. The Git metadata directory is excluded. Empty selected
directories, unsupported file types, hardlinks and unsafe paths refuse before
launch; they are not silently omitted. Live capture limits are 32 MiB per blob,
2 GiB total, 100,000 scanned entries, depth 256 and 16 MiB of selected path names.

The live snapshot uses a private synthetic Git tree and does not write objects,
refs or the index into the host repository. A retained candidate capture and a
separate mutable Stage workspace are derived from that tree. The Stage owns its
reachable Git objects without alternates to the live capture. Original host
HEAD/index, applicable Git/configuration controls, selected inventory and bytes,
and the private capture are checked before execution and after owned cleanup.
Observed drift refuses publication. All three temporary roots must be cleaned
before a successful result is published. Configured output paths must remain
outside the declared roots, tracked inputs and scratch; selected inputs cannot
be excluded to make an output path admissible.

**Declared inputs are not the complete execution dependency set.** The existing
operator read grants still make the original source/Git directories and other
granted dependencies accessible to guest code. An absolute read of an unselected
host file can affect a probe without changing the captured identity. Normal
relative source reads start in the private workspace, but this is not a hermetic
execution claim. JSON records `execution_read_scope.policy="operator_grants"`,
`original_source_readable=true` and `captured_inputs_are_dependency_closure=false`.
An input digest proves the captured preimage under the checks described here;
it does not authenticate every byte a guest may read.

These are cooperative point-in-time checks under trusted, stable operator-owned
parents. They do not authenticate guest observations or prevent a hostile
same-UID process from racing a verified file. Explicit read grants still admit
external dependencies and the source repository. The private Git repository has
the selected tree and synthetic history; it does not reproduce original history.

### Tracked inputs and automatic selection

Use `--live --tracked-only` to capture tracked HEAD/index working bytes and root
configuration without additional roots. This is an explicit alternative to
`--input`; the two cannot be combined. Untracked or ignored dependencies require
explicit input roots. Tracked-only capture does not implicitly select `.` or
scan unrelated cache/virtual-environment directories.

Choose exactly one module-selection mode:

- Repeat `--module PATH` for explicit whole modules, in either source mode.
- `--changed-from REF` selects live modules differing from the unique merge base
  of HEAD and the resolved reference, restricted to the original index inventory.
- `--all-tracked` selects existing live modules from that index inventory.

Automatic modes sort paths and exclude `test`/`tests` directory components,
`test_*.py`, `conftest.py` and non-Python files. Deleted or absent files are omitted;
staged changes reverted to base bytes are not selected. Eligible symlinked modules
are refused explicitly. Selection compares bounded captured Git objects, never a
second worktree content diff. HEAD, base, merge base, index and captured-tree
identities are bound and rechecked around execution.

Changed selection conservatively refuses Git attributes/content conversion,
submodules, special index flags and ambiguous merge bases. Live capture also
refuses `core.filemode=false`, `core.symlinks=false` and case-folded checkouts.
Git comparisons use the pinned system Git; arbitrary Git installations with
different compiled-in attribute locations are not a parity claim. These restrictions
deliberately narrow the former Python/Git selection contract; unsupported cases
fail visibly rather than silently changing which modules are evaluated.

All modules share one input/configuration capture and one confined Stage. An empty
automatic selection returns the native empty summary without starting a guest or
creating another candidate workspace. It still validates captured configuration,
limits and artifact admission, then disposes the live capture before publication.
Unused record data, waivers and guest binaries are not loaded in that case. JSON
states `execution_verified=false` and `execution_skipped="no_selected_modules"`.

```sh
make slop-gate SLOP_BASE=origin/main \
  SLOP_GRANTS=/operator/execution-grants.json \
  SLOP_GUEST_PATH=/operator/slop_core.so SLOP_GUEST_SHA256=GUEST_SHA256
```

The Make target now invokes the native live gate. `SLOP_MODULES` supplies explicit
modules; otherwise `SLOP_ALL_TRACKED=1` selects all, and the default uses
`SLOP_BASE`. `SLOP_INPUTS` supplies additional roots; its absence means tracked-only.
`SLOP_JOBS`, `SLOP_ENFORCE` and raw operator-supplied `SLOP_GATE_ARGS` control
workers, enforcement and additional native flags. Make lists are whitespace-split;
use the CLI for paths containing whitespace. No authority is inferred from these
variables. The native default is one worker, unlike the former CPU-based default.
When captured `run_artifacts` is configured, also set
`SLOP_ARTIFACTS=/absolute/existing/configured/directory`; this must exactly match
the configured destination. Omitting that output grant fails before execution,
including for an empty selection. The example above assumes no configured output.

## Configuration and authority

Both modes read `[tool.forge.equivalence_probe]` from the captured
`pyproject.toml`. Supported fields are the existing `waivers`, `ledger`, `report`,
`run_artifacts`, `per_module_timeout` and `jobs`. Ledger/report paths are used by
the separate backlog command. The module timeout defaults to 180 seconds and
must be within 0.001–3600 seconds. `--jobs` defaults to one, is bounded to 1–32,
and can be lowered by captured configuration. Workers share one confined Stage.

The grants file must be an owned, non-group/world-writable regular file outside
the source repository. It uses the existing execution-grant schema, with source
and dependency read roots, explicit `forge` and Python program entries, the
Python alias, scratch/cgroup locations, cgroup limits and execution ceilings.
The `forge` entry must identify the running executable. The guest path and digest
are explicit; the guest must be outside source/scratch and have a read grant.

Stage wall and CPU limits default to the lesser of 180 seconds and their grants;
address space defaults to the lesser of 8192 MiB and its grant. Explicit
`--wall-seconds`, `--cpu-seconds` and `--address-space-mib` must be positive and
within those ceilings. A configured batch budget must admit the Stage. Its
deadline covers execution; filesystem preparation and publication are cooperative
operations outside that deadline. Interrupts request owned cancellation, and
the command awaits cleanup before accepting a result.

If captured `[tool.forge.test_record]` enables line records, pass both `--store`
and `--store-sha256`. The path must exactly match the canonical configured path;
active SQLite WAL/journal sidecars and observed drift are refused. This is
separately supplied recorded-test data, not authenticated historical commit data.
Without enabled records, supplying a record grant is an error.

## Results and publication

Default output identifies the source mode, tree/modules and renders the native report.
`--json` returns a versioned envelope containing those identities, captured-input
evidence, Stage/resource/cleanup observations and the native summary. Live mode
also records selected roots, compact inventory/control identities, original HEAD
and index identity, and `live_capture_cleanup_complete`; `commit_oid` is null and
`scope` describes declared/tracked inputs and explicit single/plural selection.
The existing one-module `module` field remains; `modules` and `selection` describe
the complete selection and its binding. A valid
result has exit 0; `--enforce` changes this to exit 1 when blocking findings exist.
Admission, execution, cleanup and publication failures return exit 4. Argument
syntax errors return exit 2. Incomplete measurements remain visible in the
summary and are not successful task or mutation evidence.

Configured `run_artifacts` requires an explicit matching `--artifacts DIRECTORY`.
The existing directory must satisfy publication permissions. A fresh raw summary
is published there only after owned execution and capture cleanup. The envelope
identifies its path and summary digest. Publication failure is an error.
Use [native backlog folding](equivalence-backlog.md) to consume summaries.

The native gate does not implement unrestricted live-worktree access or infer
execution grants. `slop-backlog` still uses the Python gate to produce its legacy
summary file before the native fold; its remaining caller migration is separate.
See the [engineering evidence](../research/performance/forge-program-20261106-committed-equivalence.json)
for completed checks, retained failures and remaining validation. No matched
speedup, model-cost reduction or agent-task benefit is established by unit tests.
Live-input validation is tracked separately in its
[evidence index](../research/performance/forge-program-20261106-live-equivalence.json).

## Reproducing validation

The ignored `native/forge/tests/equivalence_gate_live.rs` controls require explicit
`FORGE_TEST_BIN`, `FORGE_TEST_PYTHON`, `FORGE_TEST_SLOP_EXTENSION`,
`FORGE_TEST_SLOP_SHA256` and `FORGE_TEST_CGROUP_PARENT`, plus CPU Torch/pytest.
Supply an owned bounded delegation and inspect the fixture's read-grant contract;
these controls intentionally retain evidence directories for review.
Run `cargo +1.98.0 test --release --offline --locked --manifest-path native/forge/Cargo.toml --test equivalence_gate_live -- --ignored --nocapture --test-threads=1`.
The evidence index records exact builds, comparator provenance and separate capture
measurements. Similar capture cost does not establish full-workload acceleration.

The preparation-only fixture can run without a guest or delegated cgroup. It
compares the two source modes on identical file bytes, with two warmups and ten
scored observations per mode and fixture size:

```sh
FORGE_TEST_GATE_BENCH_OUTPUT=/absolute/new-observations.json \
  cargo +1.98.0 test --release --offline --locked --manifest-path native/forge/Cargo.toml \
  --lib equivalence_gate::tests::bench::compare_committed_and_live_preparation \
  -- --exact --ignored --test-threads=1
```

The output path must be new. Release measurements use five fresh processes and
the existing performance recorder; phase timings exclude fixture setup and
observer-binary hashing. Live preparation includes three materializations and a
private Stage object store. This fixture does not launch or time a guest.
