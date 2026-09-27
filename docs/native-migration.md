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

Historical pinned measurements are preserved in [the migration history](native-migration-history.md).

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

## Latest verified commit

Commit `650b7f1205b8200b1494590ba2cde3c45e5dbf4c`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`cea5c87b2c8f83d4aafc39983157e5ead2384d47c1c25fd185d7b7e2c1692df2`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,633 | 53,724 | 204 | **50.8938% native** |
| Tests | 13,321 | 101,017 | 0 | **88.3495% Rust** |
| All tracked source, including examples | 64,954 | 154,840 | 204 | 70.3825% native |

The manifests contain 378 production files, 369 test files, and two Rust
examples, or 749 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 102 test-only ranges in
87 production Rust files: 43,893 original code lines become 30,789 after
masking. The 13,104-line delta belongs to tests. Production Rust is 66,828
raw lines minus 13,104 = 53,724; Rust tests are 87,913 separate lines plus
13,104 = 101,017.

Compared with verified `98327f4`, production native share rose 0.3078
percentage points and Rust test share rose 2.3435 points. This cohort retired
ten executable Python suites containing 122 named and 135 statically expanded
cases. Ten Rust contract targets contain 135 corresponding test functions.
The pinned tree retains 26 executable Python test suites with 534 named and
595 statically expanded cases; its 34 Python fixture inputs and shared
conftest are inventoried separately. The source commit also adds Forge's
local `check`/`verify` runner and maps invalid A2A message input to the SDK's
typed JSON-RPC error. Scoped development tests passed; the full local-check
gate was pending when this measurement was assembled. These figures measure
source composition and static inventory, not runtime coverage, behavioral
coverage, or performance. The longer-term all-Rust-test target remains open.

The [measurement evidence](native-metrics/650b7f1-evidence.tar.gz) contains
classified path manifests and hashes independently checked against all 749
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 87 masked and extracted copies,
tool hashes, the Python retirement inventory and Rust case map, reproduction
scripts, and clearly labeled scoped validation logs. The SHA-256 of its
source-content manifest is
`3d96e189ac9140b6ccf3615b29595029ece4c51ab4b171511419fd7fe3e7097e`;
the evidence archive SHA-256 is
`27d506a15330a315da80535a82392d2b52d2e15d2bbc9f002f1285131add36e7`.

## Previous verified commit

Commit `98327f462928f6d90142977c93c532d53f6c2f02`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`a7b7d7606133ccdc9911cb9e19c84fbbe2bfd94416ae3482ed766cf843b85be9`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,614 | 53,047 | 204 | **50.5860% native** |
| Tests | 15,652 | 96,196 | 0 | **86.0060% Rust** |
| All tracked source, including examples | 67,266 | 149,342 | 204 | 68.9750% native |

The manifests contain 376 production files, 367 test files, and two Rust
examples, or 745 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 100 test-only ranges in
85 production Rust files: 43,180 original Rust code lines become 30,121 after
masking. The 13,059-line delta belongs to tests. Production Rust is 66,106
raw lines minus 13,059 = 53,047; Rust tests are 83,137 separate lines plus
13,059 = 96,196.

Compared with verified `b0d8fce`, this pinned tree retires six executable
Python test suites containing 93 named and 99 statically expanded cases.
Six Rust contract targets preserve those 99 cases. Production source counts
and native share are unchanged; Rust test share rose 1.3011 percentage points.
The tree retains 36 executable Python test suites with 656 named tests and
730 statically expanded cases. Its 34 static Python fixture inputs and shared
conftest are inventoried separately. These are source composition and static
inventory measures, not runtime or behavioral coverage claims. The longer-term
all-Rust-test target remains open. Uncommitted checkout work is excluded.

The [measurement evidence](native-metrics/98327f4-evidence.tar.gz) contains
classified path manifests, hashes independently checked against all 745
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`f0bdc3b98939f08fcffcf2b5da9b6ada9f76e630f673720aeb40f12408e50f20`;
the evidence archive SHA-256 is
`aaca62ef024be08b75f07c13c07dc4d77c70d9c96366edbafd3fb7aa6d1a3e3a`.

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
