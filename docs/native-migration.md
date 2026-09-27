# Native implementation and test migration

Two independent milestones are tracked: at least 50% native production code and
at least 50% Rust test code. The longer-term test target is 100% Rust. These are
source composition metrics, not runtime coverage, behavioral coverage, or speed
claims. Adding tests cannot increase the production percentage.

## Baseline

The baseline is commit `edf2090da4eb0936ab7ad60a046c238202114d0c`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 56,559 | 39,018 | 185 | **40.7448% native** |
| Tests | 49,975 | 21,587 | 0 | **30.1655% Rust** |
| All tracked source, including examples | 106,534 | 60,704 | 185 | 36.2579% native |

## First committed cohort

Measured at commit `ed679936f413f881c07ebe39675fac9bafac141a` using the
same path classification and inline Rust test treatment as the baseline:

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 55,004 | 42,081 | 194 | **43.2581% native** |
| Tests | 47,993 | 24,318 | 0 | **33.6297% Rust** |
| All tracked source, including examples | 102,997 | 66,498 | 194 | 39.1882% native |

The production share rose 2.5133 percentage points and the Rust test share
rose 3.4642 points from the baseline. Both independent 50% milestones remain
open. These figures count committed source composition only; they do not claim
that all Python behavior has a native implementation or a native test.

The [measurement evidence](native-metrics/ed679936-evidence.tar.gz) contains
the three NUL-delimited source path manifests, Tokei 15.0.0 raw JSON for each
scope, the original and masked inline Rust files' raw Tokei JSON, the AST
range list, the AST splitter source and dependency lockfile, and tool hashes.
Its SHA-256 is
`4a8b2ca8730ecefa033008bbafbf19de9211beffc07af842131d3f684df52b7d`.
The AST splitter found 91 test-only ranges across 76 production Rust files.
Those files contained 40,448 raw Rust code lines and 28,008 after masking, so
12,440 lines were assigned to tests. The separate test-path manifest contains
11,878 Rust code lines, yielding 24,318 Rust test lines in total. The
extracted snippets' Tokei JSON is included for inspection, but its 13,073
code-line count is not added: snippet extraction changes comment
classification, while the original-minus-masked method stays additive.
The all-source view includes 99 Rust code lines in two `native/forge/examples/`
files; these are excluded from production and tests, as at baseline.

## Counting rules

Count code SLOC with **Tokei 15.0.0** against an explicit manifest derived from
`git ls-tree -r --name-only <commit>`. Only tracked implementation source belongs
in the denominator. The baseline contains Python, Rust, and shell; future C/C++
or other compiled languages must be included consistently in numerator and
denominator. Exclude generated code, dependencies, build output, data fixtures,
documentation, configuration and lockfiles. Examples are reported in the
all-source view, separately from production.

Classify paths under `tests/` or `testdata/`, Python `test_*.py`, `*_test.py` and
`conftest.py`, and Rust `*_tests.rs` or `tests.rs` as test source. Also parse Rust
with `syn` and remove test-only items from production: `#[test]`, `#[cfg(test)]`,
and compound `cfg` expressions that require `test`. A mixed `any(test, feature)`
or `not(test)` does not make an item exclusively test code. Inline test helpers
inside a test-only module belong to tests too.

Mask those items in place while preserving newlines and measure each full file
again. Attribute the difference between the original and masked Rust code count
to tests. This keeps production plus tests additive; measuring extracted snippets
alone changes Tokei's classification of some documentation comments. At baseline,
87 test-only item ranges in 72 production Rust files account for 13,112 code
lines; separate Rust test files contain another 8,475.

Count an archived commit, not a changing working directory. Record the commit,
tool versions, path manifests, AST ranges and raw Tokei JSON with every update.
Do not pad implementations, move live code into excluded paths, or remove needed
behavior to raise these percentages.

## Behavioral migration

The Forge-owned graph query CLI reads the code-review-graph SQLite index. The
external indexer remains a separate dependency; it has not been rewritten.

Forge now also has its own structural indexer: `forge graph index` parses host
Python and Rust source and writes `.forge/graph.db`. The context and reference
queries use this native snapshot, with explicit limits and stale-file checks.
The index records only structurally resolved calls; it makes no semantic
similarity claim. See [native graph behavior](native-graph.md) for commands,
resolution rules, and fallback behavior. The external index is still a
separate compatibility input when the native snapshot is absent.

Native hook routes preserve explicit partial dispatch selections and custom host
hook behavior. Compatibility entrypoints may remain thin Python bindings while
their algorithms execute in Rust. Such bindings still count as Python source.

Test retirement requires a case map and passing native replacements. See
[the test migration map](native-test-migration.md). Python process and extension
integration tests remain until their boundaries have native replacements.

`forge mutation results --adapter cargo-libtest --report results.txt
--test tests/math.rs::test_add` parses existing reports without starting a test
runner. The `pytest-junit` and `ctest-junit` adapters consume XML. Reports are
limited to 64 MiB. Exit 0 means complete attribution, 1 means incomplete
attribution, and 2 reports invalid input or an operational error. A failed test
can still have complete attribution; inspect the JSON outcomes.
