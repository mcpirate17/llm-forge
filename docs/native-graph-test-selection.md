# Native graph test selection

`forge graph-test-select` is an advisory changed-code test selector. It uses the shared native graph algorithm to report the selected source scope, graph generation, coverage/fallback reasons, Python tests, native Cargo manifests and contract targets. It never emits a governance PASS receipt.

```sh
forge graph-test-select --repo . --json
forge graph-test-select --repo . path/to/module.py --json
```

Without explicit paths, Forge discovers staged, unstaged, deleted and untracked Git paths. `--run` executes the selected plan under the native process supervisor; it cannot be combined with `--json`. For example:

```sh
forge graph-test-select --repo . --run --python /path/to/python --runtime-dir /private/existing/parent --pytest-arg=-q
```

The Python interpreter is an explicit compatibility guest, required when selected Python tests or conductor contract tests need it. Native planning and supervision remain Forge-owned. Selected conductor contracts build their required extension and Forge executable in fresh private runtime scratch; selected pytest tests use those same fresh artifacts when contracts are in the plan, rather than ambient installed extensions. `FORGE_CARGO_BIN` must name the provisioned Cargo/rustc/rustdoc directory when native Cargo tests are selected. Runtime parent must be an existing operator-owned mode-0700 directory outside the repository. Unsupported selected tests, missing prerequisites, unsafe paths, oversized change inventories, timeouts and child errors fail with a nonzero status. Scratch is removed on success; on failed execution it is retained and its path is reported for inspection. Interrupts are forwarded to supervised children and reported as signal-derived exit status.

The result is an advisory subset: graph reachability, naming conventions and fallback completeness do not prove that all relevant tests were selected or passed. Test execution does not grant review approval or replace a required gate.

Interpreter admission preserves the supplied virtual-environment executable path;
resolving its symlink to a system interpreter would lose the selected environment.
The integration fixture builds a small native extension and executable, then
checks that contract tests and pytest observe identical fresh artifact hashes.
It covers successful cleanup, pytest failure and Cargo failure with retained
scratch. This fixture does not establish that every host compatibility suite
builds or passes. `cargo bench --bench graph_test_select` separately measures
warm in-process discovery and conservative selection; it excludes process startup
and test execution and makes no before/after speedup claim.
