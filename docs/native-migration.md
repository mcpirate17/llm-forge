# Native implementation and test migration

Two independent milestones are tracked: at least 50% native production code and
at least 50% Rust test code. The longer-term test target is 100% Rust. These are
source composition metrics, not runtime coverage, behavioral coverage, or speed
claims. Adding tests cannot increase the production percentage.

Both independent 50% source-SLOC milestones are achieved at the latest verified
commit below.

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

## Earlier verified commit

Commit `e70f2d9a816d76131dbf92f14158c0ec2f5a13d0`, measured from a `git archive`
with the same path classification and inline Rust test treatment:

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 55,001 | 42,081 | 194 | **43.2594% native** |
| Tests | 46,373 | 27,114 | 0 | **36.8963% Rust** |
| All tracked source, including examples | 101,374 | 69,294 | 194 | 40.5555% native |

Production Rust contains 54,546 raw code lines before removing 12,465 inline
test lines. The AST splitter identified 91 test-only ranges in 76 production
Rust files; the separate test-path manifest contributes another 14,649 Rust
code lines. The all-source view includes 99 Rust code lines in
`native/forge/examples/`, excluded from production and tests. Compared with the
first cohort, the production share is up 0.0013 percentage points and the Rust
test share is up 3.2666 points; both 50% milestones remain open.

The [measurement evidence](native-metrics/e70f2d9-evidence.tar.gz) contains the
NUL-delimited path manifests, Tokei 15.0.0 raw JSON, inline Rust AST ranges,
original and masked inline-file counts, AST splitter source and lockfile, and
tool hashes. Its SHA-256 is
`f2ff08f2c0c51c8160c65f6a20f70d169d2be6689d45e1c7038b8cb0147b06e3`.

## Previous verified commit

Commit `f2dcdf776e3cb167006bf5bdb20cd31c165cfe1f`, measured from a `git archive`
with the same classification and inline-test treatment:

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 52,384 | 48,045 | 194 | **47.7475% native** |
| Tests | 42,101 | 37,465 | 0 | **47.0867% Rust** |
| All tracked source, including examples | 94,485 | 85,609 | 194 | 47.4846% native |

The splitter identified 94 test-only ranges across 79 production Rust files.
Those same files contain 40,889 original Rust code lines and 28,346 after
masking, giving 12,543 inline test lines. Subtracting that delta from all 60,588
raw production Rust lines gives 48,045. Adding it to 24,922 separate Rust test
lines gives 37,465. The 99 example lines remain outside both milestone scopes.

Production is up 4.4881 percentage points and Rust tests are up 10.1904 points
from the previous verified commit. Both independent 50% milestones remain open.
The new batch moves messaging transport/storage, candidate checks and policy,
campaign refresh, project path decisions, and runtime receipt decisions into
Rust. Test retirement maps are recorded in the corresponding migration documents.

The [measurement evidence](native-metrics/f2dcdf7-evidence.tar.gz) contains the
source manifests and hashes, raw Tokei JSON, AST ranges, splitter source and
lockfile, tool hashes, and reproduction script. The archived source hashes also
match an independent extraction of each committed blob. Evidence SHA-256:
`6db0524bff8a9ae155af039270dd6b0b5c5f6e69808f9474c56d68ee5e50c4c7`.

## Latest verified commit

Commit `b2ec44f7cbb9c2f6eb42960e0c710fc4cc3e0169`, measured from a `git archive`
with the same classification and inline Rust test treatment. The archive stream
SHA-256 is
`eb5864447e7bdb6ccf99e48d2c5fa0b5724614c5a40543d60bddb58f66b402aa`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,300 | 51,569 | 194 | **50.0364% native** |
| Tests | 40,566 | 41,320 | 0 | **50.4604% Rust** |
| All tracked source, including examples | 91,866 | 92,988 | 194 | 50.2507% native |

The source manifests contain 371 production files, 295 test files, and 668
source files in all. The splitter identified 100 test-only ranges in 85
production Rust files. Those same files contain 42,894 original Rust code lines
and 29,835 after masking, so the inline test delta is 13,059. Subtracting this
delta from 64,628 raw production Rust lines gives 51,569 production Rust lines;
adding it to 28,261 separate Rust test lines gives 41,320 Rust test lines. The
99 example lines remain in the all-source view and outside both milestones.

Production native share increased 2.2889 percentage points from the previous
verified commit; Rust test share increased 3.3737 points. Both independent 50%
milestones are achieved on source SLOC. These percentages do not measure runtime
coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/b2ec44f-evidence.tar.gz) contains the
source path manifests, source hashes independently checked against all 668 Git
blobs, exact archive hash, raw Tokei JSON, inline AST ranges and counts, splitter
source and lockfile, tool hashes, and reproduction script. Its SHA-256 is
`bc5d354822e23abb4d602f1d53b9fc77c4c864d74be11b22dccd4496e62b8732`.

## Behavioral migration

Forge has its own structural indexer: `forge graph index` parses host
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
