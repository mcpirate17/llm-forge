# Targeted Rust contracts for migrated Python APIs

The `python_contracts_*` Cargo tests replace focused Python tests, but Python
test selectors cannot run those Rust files. `test_contracts::plan` maps changed
production Python paths to Cargo integration-test targets. It reads the
source-and-helper-to-target registry from the selected repository or candidate
snapshot at
`native/conductor-native/src/python_contract_targets.tsv`; every present
`python_contracts_*` target must appear there, including targets whose name
does not match the Python module or which cover a reexport. Helper rows mirror
the targets' `#[path]` includes. When a cohort adds a target, add its production
source and helper rows in the same change. Hook contracts also register their
JSON templates, shell bodies, and the exact dispatch launcher path.
Rust child fixture programs under `tests/fixtures/` also map to their contract
targets. Selected fixture programs must be canonical regular files within the
snapshot, with no external module or source-splicing includes.
Checked-in JSON corpora under `native/forge/tests/fixtures/` map to the same
targets and receive canonical regular-file checks before those targets run.

The PyO3 entry point is `contract_test_plan_native(repo_root, changed_paths)`.
It returns JSON with sorted, deduplicated matched Python `source_paths`, Cargo
`targets`, repo-relative Rust `test_paths`, and `commands`. Each command has a canonical `cwd`, an `argv`
list, and matching targets and test paths. The Cargo argv uses `test --jobs 2
--offline --locked --manifest-path native/conductor-native/Cargo.toml --features
python-compat-tests`, repeated `--test <target>`, and `-- --test-threads=1`.
Callers execute the argv as a process argument list with that cwd; pytest
selection stays separate.

Changed `python_contracts_<target>.rs` files select themselves. Changed helpers
under `tests/python_contracts/` select the registered targets that include
them. A change to the registry itself selects all registered targets, so a
registry-only candidate cannot appear to have no targeted tests. For selected
targets, the planner checks the manifest, targets, and registered direct
`#[path]` helpers in the expected crate. The include set must match the helper
registry; every selected helper must be a regular file at its canonical path.
Rust syntax parsing handles literal path attributes regardless of formatting;
unregistered external modules, conditional module paths, nested helper modules,
and source `include!` are rejected. Fixture `include_str!` and `include_bytes!`
remain supported. This validates the checked-in module layout, not arbitrary
macro expansion or a security sandbox for candidate code.
It rejects path escapes and errors on a missing mapped file. It rejects
a missing, malformed, or non-regular registry in a Forge checkout. An unrelated
path in a host without Forge's native crate returns empty selection. A separate
registry inventory check detects unregistered targets in Forge's checkout;
registry-only changes also check that every on-disk target remains registered. It
does not invoke Cargo, pytest, an agent endpoint, or a model.

`native/conductor-native/tests/python_contracts_discovery.rs` checks complete inventory,
reexports and nonconvention mappings, support-file selection, path boundaries,
stable ordering, deduplication, safe argv, missing files, and PyO3 response
shape. Run that target with the crate's pinned offline Cargo settings before
wiring a caller. The candidate-review runner and standalone graph selector
consume this native plan and remain responsible for process execution,
timeouts, and reporting.

For selected contracts, both runners build the native extension and Forge
binary from the selected candidate or repository into an isolated runtime
directory. Both native Cargo manifests must be canonical regular files within
the candidate. Runtime artifact names follow this package's declared Linux
support. The test environment puts that extension and the selected `src/`
before installed Python sites, sets `FORGE_BIN` to the selected binary, and
probes the selected Python executable for PyO3 instead of inheriting an
unrelated `PYO3_CONFIG_FILE`. Cargo builds and tests stay offline with two
jobs; the standalone runner applies a 900-second wall timeout to each build
and test command. Rust integration contracts are reported separately from
pytest files. When both run under a coverage check, pytest changed-line
coverage is still evaluated and labeled `pytest-only`; a separate finding
states that Python coverage cannot measure the embedded Rust PyO3 calls.
The native percentages in [native migration](native-migration.md) measure
source composition and do not imply that coverage has been measured.
