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

## Previous verified commit

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

## Previous verified commit

Commit `8eb7c76b909f2da7e1d5e1847eb53904332fb293`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`54009c888e1bd8411a185de800d0b0663a4ed7023394eb218c58b0e81249e96b`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 40,566 | 41,398 | 0 | **50.5075% Rust** |
| All tracked source, including examples | 91,862 | 93,096 | 194 | 50.2810% native |

The manifests contain 371 production files, 295 test files, and 668 source
files total. The splitter identified 100 test-only ranges in 85 production
Rust files. Those files contain 43,010 original Rust code lines and 29,951
after masking, so the inline test delta is 13,059. Subtracting that delta from
all 64,658 raw production Rust lines gives 51,599. Adding it to 28,339 separate
Rust test lines gives 41,398 Rust test lines. The 99 example
lines remain in the all-source view and outside both milestone scopes.

Compared with `b2ec44f`, production native share increased 0.0165 percentage
points and Rust test share increased 0.0471 points. Both independent 50% source
SLOC milestones remain achieved. This hook serialization ordering fix changes
the measured source composition only; these percentages do not measure runtime
coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/8eb7c76-evidence.tar.gz) contains the
source path manifests, source hashes independently checked against all 668 Git
blobs, exact archive stream hash, raw Tokei JSON, inline AST ranges and counts,
splitter source and lockfile, tool hashes, and reproduction script. Evidence
SHA-256:
`5c16894ce99d2cc0d9a99565e4cff122e046a2fe77f5fea592230196dd2c4a2b`.

## Previous verified commit

Commit `f74113d9236c9a1f79f7605f264e9e913e57d8ac`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`09735a6c2ab17c5eeba074c0ee86d75ca11049b8da2c70e0963fdb6dbcd8a0b8`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 40,566 | 41,447 | 0 | **50.5371% Rust** |
| All tracked source, including examples | 91,862 | 93,145 | 194 | 50.2940% native |

The manifests contain 371 production files, 295 test files, and 668 source
files total. The splitter identified 100 test-only ranges in 85 production
Rust files. Those files contain 43,010 original Rust code lines and 29,951
after masking, so the inline test delta is 13,059. Production Rust remains
64,658 raw lines minus 13,059 inline-test lines = 51,599. Adding the same delta
to 28,388 separate Rust test lines gives 41,447 Rust test lines. The 99 example
lines remain in the all-source view and outside both milestone scopes.

The source change since `8eb7c76` is confined to a Rust mailbox test fixture's
TCP readiness handling. Production counts and the production native share are
unchanged; the Rust test share increased 0.0296 percentage points. Both
independent 50% source SLOC milestones remain achieved. These percentages do
not measure runtime coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/f74113d-evidence.tar.gz) contains the
source path manifests, source hashes independently checked against all 668 Git
blobs, exact archive stream hash, raw Tokei JSON, inline AST ranges and counts,
splitter source and lockfile, tool hashes, and reproduction script. Evidence
SHA-256:
`c3d66d18c9eaa07fa5f5a13a36d45b67bc159bfbec136bf4656ec4550b08ce33`.

## Previous verified commit

Commit `67976735870c79a94b8968255ce8e82a200e16b3`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`cf2b6f0bb01ef4bc67c78aab6b181048f10e06dd2b2e3d98dc18d4e721f67271`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 38,673 | 44,552 | 0 | **53.5320% Rust** |
| All tracked source, including examples | 89,969 | 96,250 | 194 | 51.6327% native |

The manifests contain 371 production files, 292 test files, and 665 source
files total, including two Rust example files outside both milestone scopes.
The splitter identified 100 test-only ranges in 85 production Rust files.
Those files contain 43,010 original Rust code lines and 29,951 after masking,
so the inline test delta is 13,059. Production Rust is 64,658 raw lines minus
13,059 inline-test lines = 51,599. Adding the same delta to 31,493 separate
Rust test lines gives 44,552 Rust test lines.

The 17-test-file migration leaves production counts and native share unchanged
from `f74113d`; the Rust test share increased 2.9949 percentage points. Both
independent 50% source SLOC milestones remain achieved. These percentages do
not measure runtime coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/6797673-evidence.tar.gz) contains the
source path manifests, source hashes independently checked against all 665 Git
blobs, exact archive stream hash, raw Tokei JSON, inline AST ranges and counts,
splitter source and lockfile, tool hashes, and reproduction script. Evidence
SHA-256:
`6a36afa8c3a460d301895f1e987fd8d92744fcfa3e7ae98682be0347d458d5c2`.

## Previous verified commit

Commit `1422203b47bd72798af6b6c0d6adff47df69c5d1`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`c15a19e57eb466ad360f9fdbb9d0661785bc01294db9925cf52c526ff010e929`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 36,780 | 48,361 | 0 | **56.8011% Rust** |
| All tracked source, including examples | 88,076 | 100,059 | 194 | 53.1299% native |

The manifests contain 371 production files, 288 test files, and 661 source
files total; the all-source view also contains two Rust example files outside
the production and test scopes. The splitter identified 100 test-only ranges
in 85 production Rust files. Those files contain 43,010 original Rust code
lines and 29,951 after masking, so the inline test delta is 13,059. Production
Rust is 64,658 raw lines minus 13,059 = 51,599. Adding the same delta to 35,302
separate Rust test lines gives 48,361 Rust test lines.

This commit retires 15 Python test modules. Production counts and the native
share are unchanged from `6797673`; the Rust test share increased 3.2691
percentage points. Both independent 50% source SLOC milestones remain
achieved. These percentages do not measure runtime coverage, behavioral
coverage, or performance.

The [measurement evidence](native-metrics/1422203-evidence.tar.gz) contains
source manifests and hashes independently checked against all 661 Git blobs,
the archive stream hash, raw Tokei JSON, inline AST ranges and counts, splitter
source and lockfile, tool hashes, and reproduction script. Evidence SHA-256:
`941a2a8ddd27cd62c750b4ea81e8c0d576fcbcf405f21916ac64e4d1f8611abb`.

## Previous verified commit

Commit `a67cc2ad2f4cf57dcc477550462fe61d620db342`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`996eb40801c2abd064317ce7d8a745487ae76caa5de3dec28c9f33620c680ca9`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 36,169 | 49,828 | 0 | **57.9416% Rust** |
| All tracked source, including examples | 87,465 | 101,526 | 194 | 53.6649% native |

The manifests contain 371 production files, 288 test files, and 661 source
files total; the all-source view includes two Rust example files outside the
production and test scopes. The splitter identified 100 test-only ranges in
85 production Rust files. Those files contain 43,010 original Rust code lines
and 29,951 after masking, so the inline test delta is 13,059. Production Rust
is 64,658 raw lines minus 13,059 = 51,599. Adding the same delta to 36,769
separate Rust test lines gives 49,828 Rust test lines.

This commit retires six Python test modules. Production counts and native
share are unchanged from `1422203`; the Rust test share increased 1.1405
percentage points. Both independent 50% source SLOC milestones remain
achieved. These are source composition figures, not runtime coverage,
behavioral coverage, or performance claims.

The [measurement evidence](native-metrics/a67cc2a-evidence.tar.gz) contains
source manifests and hashes independently checked against all 661 Git blobs,
the archive stream hash, raw Tokei JSON, inline AST ranges and counts, splitter
source and lockfile, tool hashes, and reproduction script. Evidence SHA-256:
`4ae295a221f3affa7e163d3de72eae2e5261261880393976a99f80abe88063b0`.

## Previous verified commit

Commit `63c1bea0fa56d649d35aad10b44b05b8eec968f5`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`663576eb521160540a3638eb866587cb0cce5690602000e643eb58c9d1f76b44`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 35,383 | 51,546 | 0 | **59.2967% Rust** |
| All tracked source, including examples | 86,679 | 103,244 | 194 | 54.3055% native |

The manifests contain 371 production files, 293 test files, and 666 source
files total; the all-source view includes two Rust example files outside the
production and test scopes. The splitter identified 100 test-only ranges in
85 production Rust files. Those files contain 43,010 original Rust code lines
and 29,951 after masking, so the inline test delta is 13,059. Production Rust
is 64,658 raw lines minus 13,059 = 51,599. Adding the same delta to 38,487
separate Rust test lines gives 51,546 Rust test lines.

This commit retires three Python test modules. Production counts and native
share are unchanged from `a67cc2a`; the Rust test share increased 1.3551
percentage points.
Both independent 50% source SLOC milestones remain achieved. These figures
measure source composition, not runtime coverage, behavioral coverage, or
performance.

The [measurement evidence](native-metrics/63c1bea-evidence.tar.gz) contains
source manifests and hashes independently checked against all 666 Git blobs,
the archive stream hash, raw Tokei JSON, inline AST ranges and counts, splitter
source and lockfile, tool hashes, and reproduction script. Evidence SHA-256:
`15c360f18b83de0bddf95885eb0153ca6a3bc6f1b668b7f7817ea038273ac8ed`.

## Previous verified commit

Commit `18e296e81793d12ca1041a7cc8bf3d47ec719c84`, measured from a `git archive`
with the same classification and inline-test treatment. The archive stream
SHA-256 is
`80e3e4aec3f2c887bef6c43fb189df3aefa2730d944a12f7776855077bcf984b`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 51,599 | 194 | **50.0529% native** |
| Tests | 33,391 | 54,785 | 0 | **62.1314% Rust** |
| All tracked source, including examples | 84,687 | 106,483 | 194 | 55.6442% native |

The manifests contain 371 production files, 295 test files, and 668 source
files total; the all-source view includes two Rust example files outside the
production and test scopes. The splitter identified 100 test-only ranges in
85 production Rust files. Those files contain 43,010 original Rust code lines
and 29,951 after masking, so the inline test delta is 13,059. Production Rust
is 64,658 raw lines minus 13,059 = 51,599. Adding the same delta to 41,726
separate Rust test lines gives 54,785 Rust test lines.

This commit retires six additional Python test modules since `63c1bea`; across
the current PR108 test cohort, nine Python test modules have been retired.
Production counts and native share are unchanged from `63c1bea`; the Rust test
share increased 2.8347 percentage points. Both independent 50% source SLOC
milestones remain achieved. These figures measure source composition, not
runtime coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/18e296e-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 668 Git
blobs, the archive stream hash, raw Tokei JSON, inline AST ranges and counts,
splitter source and lockfile, tool hashes, and reproduction script. Evidence
SHA-256:
`d2b85d62034586e719cf4110609ee62cfa69534adc78b964a64ca1f11f1172e8`.

## Previous verified commit

Commit `908d572d3e4cbfe646c4e0e888f0a0853b7df751`, measured from a `git archive`
with the same path classification and inline-test treatment. The archive
stream SHA-256 is
`43073b13a3abceb8118f606c254f1a11797f75e1d491976aac04ae981c5de5d2`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 52,303 | 194 | **50.3916% native** |
| Tests | 33,391 | 55,171 | 0 | **62.2965% Rust** |
| All tracked source, including examples | 84,687 | 107,573 | 194 | 55.8954% native |

The manifests contain 372 production files, 296 test files, and 670 source
files total; the all-source view includes two Rust example files totaling 99
code lines outside the production and test scopes. The splitter identified
100 test-only ranges in 85 production Rust files. Those files contain 43,180
original Rust code lines and 30,121 after masking, so the inline test delta is
13,059. Production Rust is 65,362 raw lines minus 13,059 = 52,303. Adding the
same delta to 42,112 separate Rust test lines gives 55,171 Rust test lines.

Since `18e296e`, native hook command-cwd source work added 704 production Rust
code lines and 386 Rust test code lines on this measure. Python and shell
counts are unchanged; the native production share increased 0.3387 percentage
points and the Rust test share increased 0.1651 points. Both independent 50%
source-SLOC milestones remain achieved. Dirty graph-selection and
communication test ports in the shared checkout are excluded from this
committed snapshot. These figures measure source composition, not runtime
coverage, behavioral coverage, or performance.

The [measurement evidence](native-metrics/908d572-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 670 Git
blobs, the archive stream hash, raw Tokei JSON, inline AST ranges and counts,
splitter source and lockfile, tool hashes, and reproduction script. A separate
splitter and Tokei run reproduced the ranges and per-file reports. Evidence
SHA-256:
`c9cd6468ea318a245eb0f81eb5feecc9c107caade86ff7a15464497520d424c6`.

## Latest verified commit

Commit `d8ddc76cbebb2e6a321d1a758b3f6fd423a2714b`, measured from a
`git archive` with the same path classification, Tokei 15.0.0, and AST
inline-test treatment. The archive stream SHA-256 is
`1aac265a112f97c7d19b63808384e3d88ba83ae711c93fb093f973553fe624b0`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,296 | 52,303 | 194 | **50.3916% native** |
| Tests | 32,080 | 58,105 | 0 | **64.4287% Rust** |
| All tracked source, including examples | 83,376 | 110,507 | 194 | 56.9398% native |

The manifests contain 372 production files, 299 test files, and 673 source
files total. Two Rust example files contribute 99 code lines to the all-source
view but are outside production and test scopes. The splitter identified 100
test-only ranges in 85 production Rust files. They account for 13,059 Rust
code lines: production Rust is 65,362 raw lines minus 13,059 = 52,303; Rust
tests are 45,046 separate lines plus 13,059 = 58,105.

Since `908d572`, production counts and the native production share are
unchanged. Retiring the graph-selection and communication Python tests, adding
their Rust contracts, and updating the repo-index regression reduced Python
test code by 1,311 lines and added 2,934 Rust test lines. The Rust test share
increased 2.1322 percentage points. Dirty native API, targeted-runner, and
active-state/session-close work in the shared checkout is excluded from this
committed snapshot. These are source
composition figures, not runtime coverage, behavioral coverage, or
performance claims.

The [measurement evidence](native-metrics/d8ddc76-evidence.tar.gz) includes
source path manifests and hashes independently checked against all 673 counted
Git blobs, the archive stream hash, raw and independently reproduced per-file
Tokei reports, AST inline ranges and source copies, tool hashes, and the
reproduction script. Evidence SHA-256:
`bcf055fc0b82df951dbd7b73014c4ef07dbccedeb4a5a49a124e042d3f09e7c6`.

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
