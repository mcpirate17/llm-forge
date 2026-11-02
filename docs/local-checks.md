# Native local verification

Routine verification runs against a **clean, committed** checkout. There is no
hosted CI. Delivery authorization and branch policy are separate from these
checks; a receipt does not authorize a push or merge.

```sh
make local-check
make local-verify
```

`local-check` runs the native inventory in
[`.forge/local-check.toml`](../.forge/local-check.toml): CPU and memory admission,
an isolated native installation, relocated/installed executable smoke tests,
and formatting, strict Clippy and release tests for the native libraries and CLI.
Rust 1.98.0 is selected explicitly. CUDA is masked, and Cargo workers are bounded
by the effective base policy and admitted CPU/memory capacity (up to eight with
the shipped policy). Each Rust test harness uses at most two concurrent tests,
reduced further for a one-CPU check reservation. SQLite is bundled
for the conductor and Forge builds. The initial install can fetch Cargo
dependencies; subsequent checks use `--offline --locked`.

`make build` builds the native Forge executable. `make install` installs it to
`target/install` by default; `INSTALL_ROOT=/absolute/prefix` selects another
destination. The check runner uses `.git/forge-tools/native-install`, prepends
its `bin` to `PATH`, and supplies `FORGE_BIN` and `FORGE_INSTALL_ROOT` to checks.
`FORGE_CHECK_BIN` identifies the running check executable for resource admission.
It neither installs nor discovers a Python environment. `PYO3_PYTHON` points to
an intentionally invalid path so accidental binding activation fails visibly.
The installed-runtime tests start Forge with an empty executable search path
and isolated home/configuration directories. They exercise native hook guards,
AST analysis and provider configuration without a compiler or interpreter in
the runtime environment.

The Forge Cargo workspace contains the reusable `forge` library and the
`forge-cli` executable package. Only the CLI build script stamps Git revision
and source identity, so a commit does not invalidate library and integration
test compilation. Both packages are default workspace members; use
`cargo fmt --manifest-path native/forge/Cargo.toml --all --check` to format-check
both, including the CLI build script. `make install` installs `forge-cli` while
the executable remains named `forge`.

Tests and CLI benchmarks require `FORGE_TEST_BIN` at runtime. It must name an
absolute, regular executable; missing, relative, nonexecutable and symlink paths
fail without searching for another binary. `make test` installs the release CLI
into the target directory's dedicated `test-install` prefix and supplies its
path only after installation succeeds. For a selected test or CLI benchmark:

```sh
make build
FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" \
  cargo +1.98.0 test --release --locked --manifest-path native/forge/Cargo.toml \
  --test native_session_start
```

Use the selected target directory in that path if overriding `CARGO_TARGET_DIR`.
The committed check runner resolves an explicit policy setting before an
inherited setting and passes its absolute path to installation and tests.
Without either setting, it leaves `CARGO_TARGET_DIR` unset so independent Cargo
manifests keep their separate default caches. The `cdylib`/`rlib` dependencies emit
unqualified library filenames, so their independent checks must not implicitly
share Forge's cache. `FORGE_TEST_BIN` and `FORGE_BIN` name the isolated installed
executable. Automated checks never guess a compiler output path:
Cargo target-triple configuration can change that layout. Path admission is an operator-owned test handoff,
not binary authentication; the source-bound gate builds and verifies the artifact.

Child check and preview commands remove Cargo launcher package/manifest
metadata and `OUT_DIR`, including values supplied in check environment entries.
These identify the launcher package and otherwise contaminate dependent build
fingerprints. Caller build settings such as Cargo home/target directory, job
count, compiler flags and toolchain remain available. A native re-execution
test exercises the measured command path with synthetic metadata and settings.

`make test` runs the default release test inventory for conductor-native,
slop-core and both Forge workspace packages. It also runs the explicit native
`make test-contract-registry` matrix: conductor-native library tests plus all
33 native cases in `python_contracts_discovery`, with `contract-registry` enabled
and the Python binding feature disabled. This preserves the Slop-consumer
registry assertion too. The separate `native-contract-registry` local-check step
runs that same target; native Clippy includes the feature. The gate's forbidden
`PYO3_PYTHON` remains in force.

The registry matrix uses a dedicated `contract-registry` child of the selected
conductor-native Cargo target directory: `native/conductor-native/target` by
default, or the explicit `CARGO_TARGET_DIR` when set. Its first run builds a
separate release cache; subsequent unchanged runs can reuse that cache without
replacing the default suite's unqualified `cdylib`/`rlib` outputs. Both
`make test` and the local-check matrix step use this same isolated directory.
This changes cache placement only; release settings and test coverage remain
unchanged. No cache population or measured speedup is implied.

Default native features remain `source-analysis,vector-search`; default Forge
builds do not compile the embedded legacy contract registry. The optional native
`contract-registry` feature enables it. `python` automatically enables that
feature, preserving installed Python discovery, compiled fallback identities and
missing-registry refusal. Registry parsing and refusal semantics are unchanged;
no external asset loader or new fallback was added. Explicit Python compatibility
features remain outside this native gate while their adapters are being retired;
a native PASS does not attest those adapters. Existing ignored tests retain their
stated prerequisites.

Receipts and step logs live under `.git/forge-checks/run-*/`. `forge verify`
requires the latest attempt to have passed and rechecks log hashes, committed
HEAD/tree, `origin/main`, the merge base, changed paths and the target branch's
policy. The first policy commit uses its own committed copy; once a policy
exists on the target branch, that policy governs. Updating a working-tree policy
alone does not change the authoritative gate. A failed or interrupted newer
attempt cannot be hidden by verifying an older PASS. Untracked `research/`
drafts are excluded, on the assumption that checks do not read them; the receipt
does not attest those drafts. Other untracked files and tracked changes block
the committed check.

`make local-check LOCAL_CHECK_ARGS=` selects checks from changed paths. Runner,
policy, Makefile and hosted workflow changes force the full inventory even in
that mode. Changes in either native dependency also select Forge's Clippy,
tests and installed-runtime checks. The default full invocation and
`local-verify` are the publication checks. If committed inputs or `origin/main`
change, rerun and verify the new receipt.

For uncommitted edits, `make local-preview` or `forge preview --path
native/forge/src/example.rs` prints an advisory plan. Add `--execute` to copy
versioned and nonignored source inputs into a disposable file snapshot under
`.git/forge-previews` and build the selected crate/test configuration. This is
neither a Git worktree nor a branch. It never writes a landing receipt.
Graph-selected integration targets are used when available; missing or
incomplete graph evidence is reported and affected crates remain selected.
Conductor and slop-core changes include the dependent Forge library and binary.
An optional `--filter test_name` limits preview tests only.

Preview plans list `excluded_compatibility_tests` for selected
`python_contracts_*` adapter targets; the native discovery-planner target remains
selectable despite its historical prefix, using the explicit native
`contract-registry` feature. Other preview targets keep their original native
features. They never enable Python bindings, execute an existing
`.venv/bin/python`, or build an extension overlay. Execution output
reports `native_only` and the excluded count; the receipt preserves the complete
plan. Default output summarizes graph selection; `--details` expands its path
inventory. Tests sharing a crate and feature set use one Cargo invocation.

Preview caches are locked and keyed by crate/features, native preview version,
toolchain, host and environment. An unchanged input manifest reuses the stable
cached workspace; source hash drift invalidates package artifacts even when an
edit preserved timestamps. Tests still rerun. Source changes during execution
invalidate the current-source preview receipt. Preview files never update
`.git/forge-checks/latest` and cannot pass `forge verify`.
Forge previews install the ordinary debug CLI into the locked cache's dedicated
prefix before selected tests and pass it through `FORGE_TEST_BIN`. The preview
report records CLI build/install usage and its log separately; source drift
cleans both Forge packages. Missing optional prebuilt fixtures compile on demand.

Receipt schema 2 records wall time, child CPU time, Linux `wait4` maximum process
RSS and retained/discarded log bytes per step. Logs have a 16 MiB retention cap
and incremental hashes; failure tails read at most 64 KiB. Maximum RSS is the
largest measured process, not simultaneous process-tree memory. See
[performance.md](performance.md) for numerical benchmarks and optional profilers.

Checks run serially by default. `forge check --all --jobs 2` permits at most two
independent checks; it does not select Cargo compiler workers or test threads.
The effective base policy's existing `CARGO_BUILD_JOBS` is the maximum compiler
worker count (positive decimal 1–64, absent defaults to 2). The shipped policy
requests up to eight. For maxima above two, native resource admission selects
at most the floored effective CPU capacity and one worker per 4096 MiB currently
available host/cgroup RAM, retaining the existing two-CPU/8192-MiB baseline.
Maxima one/two retain the legacy fixed plan and setup admission without a new
host probe. `RUST_TEST_THREADS` remains at most two under the shipped policy; an explicit
one-CPU check receives one test thread. Shipping
release optimization, thin LTO, codegen settings and check obligations are unchanged.

Policy `[schedule.<check-name>]` entries declare `cpus`, `resources` and
`depends_on`. Omitted CPU reservations take the whole admitted pool (at least
two CPUs for the test-thread baseline) plus the existing default
`shared-environment` resource; setup remains serial. Explicit CPU reservations
are exact: requests beyond policy authority or observed admission refuse before
setup. Each check's Cargo workers and test-thread count are capped to its CPU
reservation. Both settings use bounded positive decimal values and reject
duplicate owned environment entries. Overlapping
resource names prevent concurrency, and summed reservations cannot exceed the
admitted pool. Unknown dependencies/cycles still fail before execution; a failed
prerequisite skips dependents and prevents a blocking PASS.

The selected worker count is passed consistently through Cargo and Make;
standalone Make defaults to `CARGO_BUILD_JOBS=2` and `RUST_TEST_THREADS=2`,
and honors the selected/inherited values instead of overriding one-CPU caps.
`build-resources.json` beside the receipt records the requested/selected values
and optional resource observation. Its path and digest are printed, but it is
supplementary advisory metadata, not new approval authority or a verified
measurement receipt. Existing policy/source/step/log binding remains unchanged.
Admission observes availability; it neither reserves resources nor imposes OS
quotas. The authoritative base policy still wins over a changed HEAD policy or
inherited worker setting, so an unlanded policy edit cannot authorize its own
higher worker count. The separate scheduled sweep retains its existing resource
and two-worker policy.

The optional [sweep policy](../.forge/sweep.toml) runs native checks, installs
into a fresh prefix, exercises the installed binary, and audits all three Cargo
lockfiles with native `cargo-audit`. It requires that development tool to be
installed; unavailable tools or advisory-database failures are errors. A fresh
prefix proves relocation, not a clean-host compilation when Cargo caches are
reused. Editing the policy does not install or start a timer. Existing sweep
reports are under `research/reports/sweep/`; see [sweep.md](sweep.md) for lifecycle
commands and their effects.
