# Local checks for Forge pull requests

Forge keeps pull requests as the only route to `main`. Routine checks run on the
developer's machine against a **clean, committed** checkout. GitHub's full CI
workflow is available through `workflow_dispatch` for a clean-host fallback;
it no longer installs the full stack on every commit and pull request.

After committing the branch, run:

```sh
make local-check
make local-verify
```

`local-check` runs every step in [`.forge/local-check.toml`](../.forge/local-check.toml):
resource preflight, the cached `make install` with fresh native extensions,
Python compilation, inventory-aware legacy Python suites, Rust compatibility
contracts, installed runtime smoke, pinned duplicate/complexity/dead-code baselines, mutation
tooling canary and retention dry-run, and fmt/Clippy/tests for all three Rust
crates including the Python-free native core. It masks CUDA, caps Cargo at
two jobs, and limits each Rust harness to two concurrent tests (the Python
contracts run one at a time). jscpd 4.2.1 and PMD 7.27.0 are installed once under
`.git/forge-tools` when their checks are selected. The initial run may need
network access and the system SQLite library required by the native crates.
The core's feature configurations use separate Cargo output directories because
their `cdylib`/`rlib` artifacts would otherwise overwrite each other. Install and
test select the same directories and Python interpreter so prebuilt tests remain
usable; a shared `CARGO_TARGET_DIR` override also isolates the compatibility build
from Forge's Python-free core dependency.

The Python stages inspect tracked pytest suite paths before invoking pytest;
the conductor inventory excludes its `testdata/` inputs. A stage with no
remaining Python suites reports that inventory and leaves the Rust contracts
to their separate stage. Once the final suites are retired, both stages skip.
While Python suites remain, every pytest exit status is preserved, including
a failure to collect any cases. `make test` runs the native compatibility
contracts and all three crates' default tests, including the core's Python-free
source-analysis configuration. It uses the installed venv, masks CUDA, caps
Cargo at two jobs, and runs Python compatibility contracts serially.

The receipt and step logs are under `.git/forge-checks/run-*/`. `forge verify`
requires the latest attempt to have passed and rechecks log hashes, committed
HEAD/tree, the `origin/main` tip and merge base, changed paths, and the policy
from the target branch. The first policy commit uses its own committed copy;
after it lands, `origin/main` governs. A failed or interrupted newer attempt
cannot be hidden by verifying an older PASS. Untracked research drafts are
allowed; other untracked files and any tracked changes block the check. A
per-run Python bytecode cache prevents an older checkout's bytecode from
contaminating the result.
The receipt excludes untracked `research/` files, on the assumption that the
declared checks do not read them; it does not attest those drafts.

For a fast edit loop, `make local-check LOCAL_CHECK_ARGS=` selects checks from
the changed paths. Changes to the runner, policy, Makefile, or hosted workflow
force every check even in that mode. The full default command and
`local-verify` are the pre-PR and pre-merge gate. If `origin/main` advances,
fetch/rebase as needed, rerun the full check, and verify its new receipt.
Then push the branch, open the PR, and merge through the PR. The local receipt
is cooperative evidence on this trusted machine; GitHub does not attest it
or enforce it as a required status check.

For uncommitted edits, use `make local-preview` or `forge preview --path
native/forge/src/example.rs`. This prints an advisory plan and does not write a
landing receipt. `make local-preview LOCAL_PREVIEW_ARGS='--execute --path
native/forge/src/example.rs'` copies versioned and nonignored source inputs into
a disposable directory under `.git/forge-previews`, then builds only the
selected crate/test configuration. Graph-selected integration targets are used
when available; unavailable or incomplete graph evidence is reported explicitly
and affected crates remain selected. An optional `--filter test_name` limits the
preview test names; it never alters the full committed gate.
Default plan output reports graph metadata and selected-test counts; `--details`
expands the full graph path inventory. Tests sharing a crate and feature set are
grouped into one Cargo invocation and share one artifact cache.

Preview Cargo caches are private, locked and keyed by target, features, toolchain,
host and environment. An unchanged input manifest reuses the stable cached
workspace; source hash drift invalidates package artifacts even for edits that
preserved timestamps. Tests still rerun. Python compatibility builds import their
newly built extension through a private overlay and read the existing interpreter;
they never install a snapshot into the shared environment. Source changes during
execution invalidate the current-source preview receipt. Preview files never
update `.git/forge-checks/latest` and cannot pass `forge verify`.

Local check receipt schema 2 persists wall time, child CPU time, Linux `wait4`
maximum process RSS and retained/discarded log bytes for every executed step.
Logs are drained with a 16 MiB retention cap, hashed incrementally, and failure
tails read at most 64 KiB. Maximum RSS describes the largest measured process,
not the sum of simultaneous process-tree memory. Numerical benchmark receipts
and optional profilers are documented in [performance.md](performance.md).

The full check remains serial by default. `forge check --all --jobs 2` permits
policy-declared independent checks to overlap within a two-CPU budget. A
`[schedule.<check-name>]` table declares `cpus`, `resources` and `depends_on`.
Unspecified checks reserve both CPUs and `shared-environment`; setup always runs
serially before checks. Resource-name overlaps prohibit concurrent execution.
Unknown dependencies, cycles and impossible CPU requests fail before execution;
a failed prerequisite skips its dependent check and prevents a blocking PASS.
The same full check inventory, source binding and verification rules apply.

To run the clean-host fallback manually, use the GitHub Actions `ci` workflow
on the branch. It runs the Rust compatibility contracts and crate tests in
their own jobs; the retired Python suites are no longer invoked there.
