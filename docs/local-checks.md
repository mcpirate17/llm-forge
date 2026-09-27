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
Python compilation and tests, Rust Python-compatibility contracts, installed
runtime smoke, pinned duplicate/complexity/dead-code baselines, mutation
tooling canary and retention dry-run, and fmt/Clippy/tests for all three Rust
crates including the Python-free native core. It masks CUDA and caps Cargo at
two jobs. jscpd 4.2.1 and PMD 7.27.0 are installed once under
`.git/forge-tools` when their checks are selected. The initial run may need
network access and the system SQLite library required by the native crates.

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

To run the clean-host fallback manually, use the GitHub Actions `ci` workflow
on the branch. Its five jobs still cover the old independent environments.
