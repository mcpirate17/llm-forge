# Confined Mull campaigns

`mull-confined-v1` is an opt-in Linux profile for one changed C or C++ translation
unit and one distinct standalone test driver written in the same language. It
does not infer a project build or test command. The generated campaign pins the
source and driver bytes, uses one worker, and builds a fixed standalone target.
Unmarked Mull campaigns remain refused. The native refresh command below updates
this profile's pins while preserving its recorded survivor baseline.

## Plan and write

The selected source must be a literal changed path. The test driver must be a
different literal C/C++ translation unit of the same language. Use `--extra-test`
to bind that explicit pair:

```sh
forge mutation plan cpp --repo-root /absolute/repository --owner my-lane \
  --base origin/main --only src/subject.cpp --mull-confined --jobs 1 \
  --run-timeout 60 --extra-test src/subject.cpp=tests/driver.cpp
forge mutation write cpp --repo-root /absolute/repository --owner my-lane \
  --base origin/main --only src/subject.cpp --mull-confined --jobs 1 \
  --run-timeout 60 --extra-test src/subject.cpp=tests/driver.cpp
```

To update the same source and driver after an edit, use the exact manifest path
returned by `write`, not a campaign ID:

```sh
forge mutation refresh campaigns/PATH-FROM-WRITE.json --repo-root /absolute/repository
forge mutation refresh campaigns/PATH-FROM-WRITE.json --repo-root /absolute/repository --apply
```

Refresh previews by default. `--apply` replaces only changed source/driver hashes;
all other manifest fields, including survivor-ratchet metadata, are preserved.
Current pins are a no-op, leaving the existing bytes and inode unchanged.
The JSON report includes old/proposed manifest hashes and both input pins;
`--verbose` adds the complete proposed manifest. This command supports only
`mull-confined-v1`. It does not check registration, change the registry, execute
tests or Mull, or issue a receipt. Existing receipts still require fresh validation;
the output's `NOT_EVALUATED` evidence status is not a passing result.

The command captures bounded manifest/source/driver bytes, rejects symlinks and
hard links, and rechecks their identities and hashes immediately before replacing
the manifest. Cooperating publishers serialize on the parent-directory lock.
This detects observed drift, not hostile same-UID changes between checks and
rename. A directory-sync failure is reported even if replacement already occurred.

Review the plan and exact hashes in the written manifest. C sources use C11;
C++ sources use C++17, without compiler extensions. The generated test command is
the private `forge-mull-test` executable. The fixed build uses CMake 3.28 or newer,
Ninja, Clang/LLVM 18, the pinned Mull pass plugin, `llvm-profdata`, and the pinned
linker. It does not execute project build definitions or infer project-specific
flags. Snapshot inputs are still read and hashed for reproducibility. Build paths
must be canonical simple ASCII paths without spaces or metacharacters.

## Pin the toolchain and run

Prepare an operator-owned grant document and pin its exact bytes with
`--execution-grants` and `--execution-grants-sha256`, following the grant format in
[mutation.md](mutation.md#ordinary-cargo-execution-grants). For this profile,
`programs` must contain exactly these path-and-SHA-256 aliases: `cmake`, `ninja`,
`clang`, `ld`, `llvm-profdata`, `mull-runner`, `sh`, and `uname`. The LLVM major
must be 18.

The `mull_toolchain` object contains `llvm_major`, `plugin`, `gcc_install_dir`,
`files`, and `directories`. Pin the Mull plugin as an exact file. Pin exact files
for GCC installation inputs `crtbeginS.o`, `crtendS.o`, and `libgcc.a` beneath
`gcc_install_dir`. That field is required metadata used for explicit compiler
selection; it does not grant recursive access to the directory. List other needed
files or runtime data directories explicitly, with SHA-256 digests and matching
`read_only` entries. Data pins allow at most 256 files, eight inventoried
directories, 256 MiB per file, 1 GiB total data, and 20,000 inventory entries.
The shared grant limit of 120 read roots also applies. Program limits are separate.
Inventories reject symlinks, special files, and unsafe permissions. Program aliases
constrain Forge launches, not every descendant executable.

Obtain a directory digest with:

```sh
forge mutation mull-inventory --directory /canonical/path/to/data
```

This command reports a bounded point-in-time content inventory; it grants no
execution or read authority. Do not put a broad system directory in `read_only`.
The grant and toolchain inventory are held and checked around the run. This is
persistent-change detection, not protection from transient same-user changes or
producer attestation.

Choose an existing delegated cgroup v2 parent, a private scratch parent, and
explicit CPU, memory, process-count, and wall-time bounds appropriate for the
standalone build. Forge runs the build, baseline, coverage merge, and Mull runner
under its supervised child owner and records resource and cleanup observations in
the receipt. Children have network access denied. A private child creation mask
(`0077`) is selected without changing the parent process mask. This does not make
an arbitrary test driver trustworthy; review the driver and toolchain pins.

Run the manifest returned by `write`:

```sh
forge mutation run campaigns/PATH-FROM-WRITE.json \
  --repo-root /absolute/repository --owner my-lane --base origin/main \
  --only src/subject.cpp --allow-mutations \
  --receipt campaigns/receipts/my-run.json \
  --execution-grants /operator/private/grants.json \
  --execution-grants-sha256 SHA256_OF_EXACT_GRANT_BYTES
```

The run requires a successful baseline before mutation scoring. A failed build or
baseline cannot produce PASS. Inspect the receipt's source and driver pins,
toolchain identity, standalone executable hash, baseline observations, coverage
profile, retained Mull report, per-mutant outcomes, cleanup ledger, and survivor
accounting. The report covers the single standalone executable; it does not
measure project-wide coverage or establish exhaustive mutation operators.
Survivors and incomplete evidence are not PASS. A clean receipt is evidence for
this exact authored pair, not approval of a candidate or support for other C/C++
projects.

## Optional real CLI controls

The operator-only fixture configuration is separate from the execution grant. It
has `schema_version: 1`, the exact `programs` map, a pinned `setup_git` program,
`read_only`, `cgroup_parent`, and `mull_toolchain`. Pin the configuration bytes and
provide a verified release binary. These ignored controls create authored fixture
repositories; they do not install tools or create cgroup delegation:

```sh
FORGE_TEST_BIN=/absolute/path/to/verified/forge \
FORGE_TEST_MULL_CONFIG=/operator/private/mull-fixture.json \
FORGE_TEST_MULL_CONFIG_SHA256=SHA256_OF_EXACT_CONFIG_BYTES \
  cargo +1.98.0 test --release --offline --locked \
  --manifest-path native/forge/Cargo.toml --test mutation_run_cli \
  confined_cli::real::mull:: -- --ignored --nocapture --test-threads=1
```

The suite checks strong and weak automatic campaigns, a failed-baseline refusal,
and a deliberately wrong-plugin pin that must refuse before payload execution.
The failed-baseline driver forks a detached child with a 30-second backstop. A
marker, pipe handshake, and live-child check precede leader exit; prompt completion
and whole-cgroup removal provide lifecycle evidence, not independent PID
attestation. Retain command ledgers, raw receipts, toolchain hashes, and cleanup
observations for review. A refusal-only control does not establish a valid build
or mutation campaign.

## Current evidence and limits

At runtime `f443a20`, the strong control returned PASS with one automatically
selected `cxx_add_to_sub` mutant killed. The weak control returned
`RATCHET_HELD` with one survivor; it is not PASS. A deliberately wrong plugin pin
was refused before payload execution. The RED baseline-failure control completed
the build, then returned `BASELINE_FAILED` when the baseline leader exited 1 after
the detached-child handshake. Owned cleanup completed in 0.02 seconds, before the
child's 30-second backstop. These are narrow authored controls.

Independent retained-artifact review found no discrepancy in the scoped
observations. Before parent retirement, fresh checks found all 15 command cgroups,
three snapshots, and three generated definitions absent while the parent cgroup
was still live. The detached-child signal/marker lifecycle was observed; individual
child PID and start-time attestation was not captured.

A later freshness exercise used a new policy and automatic run that added one kill;
it preserved the old receipt, identified it as stale, and explicit evidence
verification returned FAIL (5). The 15-phase, 36-command live and committed
freshness matrix returned CLEAN / REGRESSED / CLEAN and evidence PASS / FAIL (5) /
PASS. Independent audit verified all 45 preparation/probe commands, receipt pins,
Git changes and exact restoration. The additional automatic run's six command
groups, snapshot and definition were absent before supervisor retirement. This
sequence checks receipt acceptance within a committed snapshot; it does not
approve the whole candidate.

Two earlier real-control attempts remain part of the evidence history: `3402fe7`
stopped during GCC toolchain discovery; `c1e9e3f` configured and built but failed
private executable admission before baseline or mutation. The corrected runtime
completed the controls above; preserving those failures makes the sequence
auditable.

The full 18-step gate and exact-head/current-base verification passed at `b09b5d8`:
5,000 passing executions, zero failures and 203 ignored across 182 harnesses.
Its installed source fingerprint matches the implementation used for the real
controls. Subsequent publication heads require their own matching receipts.
The next gate at `68caf0a` found an unrelated embedding HTTP fixture race.
A test-only complete-request reader correction retained the exact error assertion;
12 focused tests and 200 repeated duplicate-key cases passed. The failed gate is
retained. All 18 checks and fresh-base verification subsequently passed at
`a154c27`, again with 5,000 passing executions, zero failures and 203 ignored.
[PR #215](https://github.com/mcpirate17/llm-forge/pull/215) merged as `de687f5`,
with exactly the checked tree. This is the runner's gate, not the later refresh
increment's publication receipt.
No global installation, host adoption, performance improvement, or agent-task benefit
is established. See the
[Mull evidence index](../research/performance/forge-program-20261106-confined-mull.json)
for run identities, receipts, limits, and publication status.
