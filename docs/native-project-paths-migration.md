# Native project path resolution

`conductor.project_paths` now delegates host path decisions to
`conductor_native::project_paths`. The Rust core reads each host's
`pyproject.toml`, resolves eleven independent path keys with environment,
`[tool.conductor]`, then default precedence, and returns each normalized
relative path with its configured flag. It also resolves integration branch
history, worktree pattern values, nearest Git ancestors, host roots, and the
inverse package-to-tree root lookup. Answers remain call-time values because
the root, environment, and host manifest can differ between calls.

The Python module keeps the public constants, `ProjectPaths` dataclass,
`Path`/`PurePosixPath` return types, and existing accessors. `conductor_table`
continues to use `tomllib` as a public mapping reader for consumers of
arbitrary host keys. Python `re.compile` remains the final syntax check for
worktree patterns because host patterns use Python regex syntax; Rust owns
the list shape, item type, trimming, and precedence. Imports of `_native` are
lazy to keep foundational imports acyclic. A malformed or unreadable manifest
is replayed through `conductor_table` only on the error path so callers still
receive the original `tomllib` or OS exception type.

The pure Rust test in `native/conductor-native/tests/project_paths.rs` covers
normalization, defaults, configured flags, malformed values, branch history,
and discovery. The Rust-owned Python boundary suite
`native/conductor-native/tests/python_contracts_project_paths.rs` exercises
the retained Python API across the cases mapped in
`docs/native-boundary-test-migration.md`. The original Python tests remain
until both native and hosted boundary suites pass against the rebuilt
extension. No existing Python test has been removed in this migration.
