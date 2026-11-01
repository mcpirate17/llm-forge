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
and formatting, strict Clippy and release tests for all three Rust crates.
Rust 1.98.0 is selected explicitly. CUDA is masked, Cargo uses at most two jobs,
and each Rust test harness uses at most two concurrent tests. SQLite is bundled
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

`make test` runs the default release test inventory for conductor-native,
slop-core and Forge. Explicit Python compatibility features remain outside this
native gate while their adapters are being retired; a native PASS does not
attest those adapters. Existing ignored tests retain their stated prerequisites.

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
selectable despite its historical prefix. They never enable Python bindings, execute an
existing `.venv/bin/python`, or build an extension overlay. Execution output
reports `native_only` and the excluded count; the receipt preserves the complete
plan. Default output summarizes graph selection; `--details` expands its path
inventory. Tests sharing a crate and feature set use one Cargo invocation.

Preview caches are locked and keyed by crate/features, native preview version,
toolchain, host and environment. An unchanged input manifest reuses the stable
cached workspace; source hash drift invalidates package artifacts even when an
edit preserved timestamps. Tests still rerun. Source changes during execution
invalidate the current-source preview receipt. Preview files never update
`.git/forge-checks/latest` and cannot pass `forge verify`.

Receipt schema 2 records wall time, child CPU time, Linux `wait4` maximum process
RSS and retained/discarded log bytes per step. Logs have a 16 MiB retention cap
and incremental hashes; failure tails read at most 64 KiB. Maximum RSS is the
largest measured process, not simultaneous process-tree memory. See
[performance.md](performance.md) for numerical benchmarks and optional profilers.

Checks run serially by default. `forge check --all --jobs 2` allows independent
checks to overlap within a two-CPU budget. Policy `[schedule.<check-name>]`
entries declare `cpus`, `resources` and `depends_on`. Unspecified checks reserve
both CPUs and `shared-environment`; setup always runs serially. Overlapping
resource names prevent concurrency. Unknown dependencies, cycles and impossible
CPU requests fail before execution. A failed prerequisite skips dependents and
prevents a blocking PASS.

The optional [sweep policy](../.forge/sweep.toml) runs native checks, installs
into a fresh prefix, exercises the installed binary, and audits all three Cargo
lockfiles with native `cargo-audit`. It requires that development tool to be
installed; unavailable tools or advisory-database failures are errors. A fresh
prefix proves relocation, not a clean-host compilation when Cargo caches are
reused. Editing the policy does not install or start a timer. Existing sweep
reports are under `research/reports/sweep/`; see [sweep.md](sweep.md) for lifecycle
commands and their effects.
