# Native committed-module equivalence gate

`forge equivalence gate` probes one whole Python module from an explicit Git
commit using the existing confined native execution owner. Forge orchestration
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

The required commit is a full lowercase object identity resolving to a commit;
branch names, tags, abbreviations and tree objects are refused. The module is one
literal relative regular `.py` blob. Selection covers the whole module, including
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

These are cooperative point-in-time checks under trusted, stable operator-owned
parents. They do not authenticate guest observations or prevent a hostile
same-UID process from racing a verified file. Explicit read grants still admit
external dependencies and the source repository. The private Git repository has
the selected tree and synthetic history; it does not reproduce original history.

## Configuration and authority

The command reads `[tool.forge.equivalence_probe]` from the captured
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

Default output identifies commit/tree/module and renders the native report.
`--json` returns a versioned envelope containing those identities, captured-input
evidence, Stage/resource/cleanup observations and the native summary. A valid
result has exit 0; `--enforce` changes this to exit 1 when blocking findings exist.
Admission, execution, cleanup and publication failures return exit 4. Argument
syntax errors return exit 2. Incomplete measurements remain visible in the
summary and are not successful task or mutation evidence.

Configured `run_artifacts` requires an explicit matching `--artifacts DIRECTORY`.
The existing directory must satisfy publication permissions. A fresh raw summary
is published there only after owned execution and capture cleanup. The envelope
identifies its path and summary digest. Publication failure is an error.
Use [native backlog folding](equivalence-backlog.md) to consume summaries.

This command does not replace the Python gate's live-worktree, default-changed or
`--all` selection. Existing Make gate callers retain their current contract.
See the [engineering evidence](../research/performance/forge-program-20261106-committed-equivalence.json)
for completed checks, retained failures and remaining validation. No matched
speedup, model-cost reduction or agent-task benefit is established by unit tests.
