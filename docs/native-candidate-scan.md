# Native candidate source scan

`forge candidate scan --check NAME --repo SNAPSHOT --base FULL_BASE_OID --tree FULL_TREE_OID --execute -- FILE...`
runs one native scan against an exact candidate snapshot. Supported `NAME` values are `style-scan`, `rust-style-scan`, `hot-loop-scan`, `test-value-scan`, and `verbosity-scan`. Remove `--execute` to inspect the bound selection plan without running a scanner. `forge candidate scan --check NAME --tool-version` reports the native implementation version and does not open Git.

The `SNAPSHOT` path is the materialized candidate tree, with the private Git directory, work tree and index supplied by Forge's candidate Stage. `--base` and `--tree` must be full Git object IDs admitted by that context. Candidate policy commands should use `{snapshot}`, `{base}`, `{tree}`, and `{files}`. The dry plan does not execute a detector. Use `--execute` through the parent candidate Stage for the operator's timeout, cgroup, environment and filesystem controls; invoking the child CLI directly is not an independent sandbox.

## Scope

The candidate scanner computes inclusive changed-line ranges from immutable base and candidate Git objects. It does not call Git or reparse diff output. `style-scan`, `rust-style-scan`, and `hot-loop-scan` retain findings whose reported start line is changed. `test-value-scan` and `verbosity-scan` retain findings whose reported span overlaps a changed line. A changed body can therefore keep a whole-function finding even when the finding starts on the function declaration.

Deleted files and files with no new-side changed lines receive explicit non-scanned scope/reason output. Unsupported suffixes and paths outside configured hot folders are excluded explicitly. Missing/unreadable eligible sources and malformed scan inputs make execution incomplete. `scanned_file_count` distinguishes an empty selection from analyzed content. The implementation never treats truncating a result list as a clean scan.

`hot-loop-scan` reads captured `pyproject.toml` configuration at `[tool.forge.hot_paths] paths`. The value is an exact list of relative folders; a missing table is an explicit empty hot-folder scope. Present settings require only the `paths` key, with 1–64 unique normalized relative paths (each at most 4096 bytes); malformed TOML, extra keys, duplicates and unsafe paths fail closed. Other scan modes do not depend on hot-path configuration.

## Bounds and result codes

Selection admits 1–4096 unique captured change paths. Sources selected for AST analysis are limited to 1 MiB each; captured files with no new-side changed lines may be up to 8 MiB. Captured source bytes are capped at 32 MiB aggregate; the separate captured configuration is at most 1 MiB. At most 4096 findings are retained; exceeding that cap makes the attempt incomplete. This is an output cap: detector intermediate allocations are subject to the parent Stage's memory and time limits. Input verification rereads captured files, so the source-byte count is not total I/O.

Exit status is `0` when execution completes with no findings, `1` when one or more findings remain, and `2` when admission, reading, parsing, or result limits fail. The parent Stage separately reports signals, timeouts and resource failures. A dry plan does not establish that detectors ran or passed. Report findings and incomplete reasons to the caller; do not convert an incomplete result into a pass.

## Full-file inspection

For a full-file inspection that is not tied to a candidate base/tree, use the separate native analysis command, for example:

```sh
forge analyze --root ROOT scan --checks style path/to/file.py
forge analyze --root ROOT scan --checks rust path/to/file.rs
```

`forge analyze scan` reports checks and source hashes for selected files but has no candidate changed-line scope. Use the candidate command for policy evaluation on changed lines.

The candidate scan reuses the native `slop-core` detector implementations. The Python `style_scan`, `diff_ranges`, hot-loop, test-value, verbosity wrappers and `hot_paths_config` adapters have been retired together. Rust test files retain the detector's path exclusions (`tests/`, `benches/`, `_test.rs`, `_tests.rs`); a `test_` filename prefix alone is not excluded.

Focused release validation passed: 12 candidate CLI cases, five configuration/scope unit tests, nine full-file analysis regressions, and 182 Slop tests. Two explicitly enabled Stage cases passed with delegated cgroups, verifying changed-line findings, resource containment, cleanup and original source/index preservation. Tests run with an explicit native Forge executable; CLI cases clear PATH and forbid Python. Full committed-tree validation is recorded separately by the native local gate.

The [current-only measurements](../research/performance/native-source-scan-20261101.json) bind benchmark and detector source hashes. The shared five-check scan of 128 synthetic Python functions took 4693.292 microseconds median sample mean (4759.061 at p95). A 300-array-binding hot-loop input took 2155.263 microseconds (2228.718 at p95). These include parsing and detector work, exclude Git/filesystem/process/sandbox costs, and do not establish a baseline speedup or microsecond end-to-end tool latency.
