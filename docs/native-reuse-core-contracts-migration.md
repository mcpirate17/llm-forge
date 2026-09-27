# Reuse inventory, ROI, consolidation, and file-family contracts in Rust

Four complete Python modules under `src/conductor/reuse/` contribute nine named
cases to this cohort: `test_native_reuse_inventory.py` (3), `test_roi_native.py`
(2), `test_consolidation_native.py` (2), and `test_file_families_native.py` (2).
Their focused pytest baseline passed 9/9 in `/tmp/forge-reuse-four-baseline.log`.

The replacement Rust targets in `native/conductor-native/tests/` have matching
`python_contracts_reuse_{inventory,roi,consolidation,file_families}.rs` names.
They call the production Python adapters and native extensions through PyO3;
all fixture setup, expected values, fake graph-index methods, SQLite rows, and
assertions are Rust-owned. The file-family fixture retains the original seeded
Python `Random.sample` calls, driven by Rust, so it presents the same 15
profiles and 105 pair universe. No Python test algorithm or retired pytest
function is wrapped.

The inventory target exercises `graph_index.py`, `repo_evidence.py`, and the
native reuse matcher. ROI exercises the candidate's native repository scan
and Python snapshot policy. Consolidation covers native clustering, ranking,
IDs, suggested homes, and token-clone evidence. File families covers native
pair scoring, safe candidate bounds, and complete-link groups. All four
targets import `conductor.reuse`, which loads `slop_core` once; candidate and
standalone contract runs must therefore build `slop_core` from the selected
checkout and place it before installed packages on `PYTHONPATH`.

Independent source-to-Rust review passed all nine named cases, including the
seeded profile values, exact Python container comparisons, graph-index method
signatures, and candidate-local `slop_core` dependency. The four original
Python modules were then retired. Historical campaign path/hash references
retain their original provenance and are not executable imports or replacement
coverage.

With the four originals retired, focused Rust checks passed inventory 3/3,
ROI 2/2, consolidation 2/2, and file families 2/2 under the candidate-built
native55 extension in `/tmp/forge-reuse-four-post-retire-tests.log`. Scoped
Clippy passed with warnings denied in
`/tmp/forge-reuse-four-post-retire-clippy.log`. Contract discovery passed its
24 focused cases after all direct provider/helper rows were registered.
