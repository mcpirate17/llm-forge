# Candidate-review benchmark input after Python test retirement

The `small-python` and `full-review` latency scenarios stage the same one-line
docstring change to the shipped `conductor/candidate_review/benchmark.py` module.
The benchmark fixture already copies that module and claims the
`conductor/candidate_review` path. Rust checks that the expected docstring occurs
exactly once before it returns the changed bytes. A missing source or changed
marker fails the fixture before either timed review.

The fixture no longer requires the five retired top-level Python test suites.
The `docs-only`, `native-code`, `dependency`, and `large-delete-rename` scenario
inputs remain as before. The changed Python path is now production code rather
than `conductor/test_candidate_review.py`; previous `small-python` and
`full-review` timings measure a different input and must not be compared as a
matched before/after performance result. Re-run both scenarios on the same
shipped version when evaluating candidate-review latency.
