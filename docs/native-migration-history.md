# Native migration measurement history

Earlier source-pinned measurements and their evidence links. The current baseline,
counting rules, latest verified commit, and behavioral notes remain in
[the native migration overview](native-migration.md).

## Verified `98327f4` cohort

Commit `98327f462928f6d90142977c93c532d53f6c2f02` was measured from a
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

Compared with verified `b0d8fce`, this pinned tree retired six executable
Python test suites containing 93 named and 99 statically expanded cases.
Six Rust contract targets preserve those 99 cases. Production source counts
and native share were unchanged; Rust test share rose 1.3011 percentage points.
The tree retained 36 executable Python test suites with 656 named tests and
730 statically expanded cases. Its 34 Python fixture inputs and shared
`conftest.py` were inventoried separately.

The [measurement evidence](native-metrics/98327f4-evidence.tar.gz) contains
classified path manifests, hashes independently checked against all 745
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`f0bdc3b98939f08fcffcf2b5da9b6ada9f76e630f673720aeb40f12408e50f20`;
the evidence archive SHA-256 is
`aaca62ef024be08b75f07c13c07dc4d77c70d9c96366edbafd3fb7aa6d1a3e3a`.

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

## Previous verified commit

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

## Previous verified commit

Commit `b49591d5ddaed896c91ab5df398e59dce0f78c0d`, measured from a
`git archive` with the same path classification, Tokei 15.0.0, and syn-based
inline Rust test treatment. The archive stream SHA-256 is
`ce0250abf420411a1cc1dbe77c186919dc7a305b8844d6700f3b8a4a8b08666f`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,577 | 52,892 | 194 | **50.5355% native** |
| Tests | 31,406 | 60,904 | 0 | **65.9777% Rust** |
| All tracked source, including examples | 82,983 | 113,895 | 194 | 57.7936% native |

The manifests contain 375 production files, 301 test files, and 678 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter identified 100 test-only ranges in 85 production
Rust files. Those files contain 43,180 original Rust code lines and 30,121
after masking, so the 13,059-line inline test delta is assigned to tests.
Production Rust is 65,951 raw lines minus 13,059 = 52,892; Rust tests are
47,845 separate lines plus 13,059 = 60,904.

Since `d8ddc76`, native contract discovery and candidate-local execution added
589 production Rust code lines, alongside 281 Python lines; the native
production share rose 0.1439 percentage points. Retiring five Python test
modules and the dispatch-only conftest, adding their Rust contracts, and adding
discovery and runner tests reduced Python test code by 674 lines and added
2,799 Rust test lines. The Rust test share rose 1.5490 percentage points.
The migrated suites represent 41 named Python cases and 47 expanded
executions; 41 Rust tests replace them, with 19 discovery and 7 runner tests
added separately. Both independent 50% source-SLOC milestones remain
achieved. These percentages do not measure runtime coverage, behavioral
coverage, or performance. Uncommitted work in the shared checkout is excluded.

The [measurement evidence](native-metrics/b49591d-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 678 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both the measurement and
verification scripts. Its SHA-256 is
`481a9a66c4e8c0db23b336812b744fd02dd995dc50cffad8cd14660aba0b8c41`.

## Previous verified commit

Commit `10c5ac62997b6cb0f44f543d6b1f41829d02ef35`, measured from a
`git archive` with the same path classification, Tokei 15.0.0, and syn-based
inline Rust test treatment. The archive stream SHA-256 is
`1fad8ea6a00e6129b76b62a93f250e78b696d4277984861f4283dfc4ea44933b`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,892 | 194 | **50.5196% native** |
| Tests | 31,406 | 60,904 | 0 | **65.9777% Rust** |
| All tracked source, including examples | 83,016 | 113,895 | 194 | 57.7839% native |

The manifests still contain 375 production files, 301 test files, and 678
counted source files. Two Rust example files contribute 99 code lines only to
the all-source view. The splitter again found 100 test-only ranges in 85
production Rust files. Those files contain 43,180 original Rust code lines and
30,121 after masking, assigning a 13,059-line delta to tests. Production Rust
is 65,951 raw lines minus 13,059 = 52,892; Rust tests are 47,845 separate lines
plus 13,059 = 60,904.

Since `b49591d`, the reviewed targeted-runner helper extraction adds 33 Python
production code lines in `verification.py`. Counted Rust code, test code, and
source paths are unchanged. The native production share falls 0.0159 percentage
points; both independent 50% source-SLOC milestones remain achieved. These
percentages do not measure runtime coverage, behavioral coverage, or
performance. Uncommitted work in the shared checkout is excluded.

The [measurement evidence](native-metrics/10c5ac6-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 678 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both the measurement and
verification scripts. Its SHA-256 is
`5d2dfcc9e05ee74a1401ec40fc6f1cecb7fb2331ca93b25dd9845abc0a37bd6f`.

## Previous verified commit

Commit `bc8fdefa1ed6fcc405d3b90237489a89883317fe`, measured from a
`git archive` with the same path classification, Tokei 15.0.0, and syn-based
inline Rust test treatment. The archive stream SHA-256 is
`6c2ef14d62afee7b2f1ec0f241dbb3c2edac83109873b3e5af0795666204fb0c`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,909 | 194 | **50.5276% native** |
| Tests | 28,859 | 67,216 | 0 | **69.9620% Rust** |
| All tracked source, including examples | 80,469 | 120,224 | 194 | 59.8466% native |

The manifests contain 375 production files, 311 test files, and 688 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 65,968 raw
lines minus 13,059 = 52,909; Rust tests are 54,157 separate lines plus 13,059
= 67,216.

Since `10c5ac6`, the pinned cohort retired 12 Python test modules and added
134 named Rust-owned contract tests across 13 targets. The post-tool corpus
and Bash parity targets also exercise 44 and 64 fixture rows respectively.
Production native share rose 0.0080 percentage points and Rust test share rose
3.9843 points. Both independent 50% source-SLOC milestones remain achieved.
These percentages do not measure runtime coverage, behavioral coverage, or
performance. Uncommitted work in the shared checkout is excluded.

The [measurement evidence](native-metrics/bc8fdef-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 688 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`b5e9e762d4b7c87354d7e06f2b32299214813bc7b7b3ff9e02e6cc2820ad4a94`.

## Previous verified commit

Commit `4da207110e81697b1591a08285285598f7b8991a`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`ed11a393ebabbe7ac2a4cb8db50f1de290dfa33739c1136a1215528bc8dd5e78`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,909 | 194 | **50.5276% native** |
| Tests | 27,948 | 69,657 | 0 | **71.3662% Rust** |
| All tracked source, including examples | 79,558 | 122,665 | 194 | 60.6001% native |

The manifests contain 375 production files, 315 test files, and 692 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 65,968 raw
lines minus 13,059 = 52,909; Rust tests are 56,598 separate lines plus 13,059
= 69,657.

Since `bc8fdef`, this pinned cohort retired four Python test modules and added
60 named Rust-owned contracts across five targets, preserving 69 expanded
pytest baseline cases. Production source counts and its 50.5276% native share
are unchanged. Rust test share rose 1.4042 percentage points. Both independent
50% source-SLOC milestones remain achieved. These percentages do not measure
runtime coverage, behavioral coverage, or performance. Uncommitted work in
the shared checkout is excluded.

The [measurement evidence](native-metrics/4da2071-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 692 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`12ec44ba3398749a7da1e6aa89c851a27ce4dd950173354a4caffbf2ea199956`.

## Previous verified commit

Commit `c0ab4e080f661443085628d8035122b4e2823e92`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`324194d8f06a7ec168d21e0ba6e894c31dc4ee1142fb3c9c6df2d5d21bf0e3d0`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,917 | 194 | **50.5314% native** |
| Tests | 26,936 | 71,993 | 0 | **72.7724% Rust** |
| All tracked source, including examples | 78,546 | 125,009 | 194 | 61.3544% native |

The manifests contain 375 production files, 319 test files, and 696 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 65,976 raw
lines minus 13,059 = 52,917; Rust tests are 58,934 separate lines plus 13,059
= 71,993.

Since `4da2071`, this pinned cohort retired four Python test modules and added
53 named Rust-owned contracts across four targets plus a test-only Rust stdio
fixture. The native contract registry now maps bounded top-level Rust source
dependencies, adding one discovery case; these mappings do not establish
behavioral coverage of the Rust implementation. Production Rust rose eight
code lines, while production Python and Shell counts remained unchanged. Rust
test share rose 1.4062 percentage points. Both independent 50% source-SLOC
milestones remain achieved. These percentages do not measure runtime coverage,
behavioral coverage, or performance. Uncommitted work in the shared checkout
is excluded.

The [measurement evidence](native-metrics/c0ab4e0-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 696 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`e6458cfd2bca0f72cc8339de33e0752126c618419891106b1fd4fc501989eff6`.

## Previous verified commit

Commit `f8a1af1a4194eaec77e7dfcc0e7a997d47c69e95`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`88e6ac05a327e1a1cdd9f3c245712cc9ac01036caddaba5a69f0e0bbf2b7a563`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,917 | 194 | **50.5314% native** |
| Tests | 25,993 | 73,774 | 0 | **73.9463% Rust** |
| All tracked source, including examples | 77,603 | 126,790 | 194 | 61.9736% native |

The manifests contain 375 production files, 323 test files, and 700 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 65,976 raw
lines minus 13,059 = 52,917; Rust tests are 60,715 separate lines plus 13,059
= 73,774.

Since `c0ab4e0`, this pinned cohort retired three Python test modules and
added 46 named Rust-owned contracts across four targets, plus a test-only Rust
interpreter fixture. Production source counts and its 50.5314% native share
are unchanged. Rust test share rose 1.1739 percentage points. Both independent
50% source-SLOC milestones remain achieved. These percentages do not measure
runtime coverage, behavioral coverage, or performance. Uncommitted work in
the shared checkout is excluded.

The [measurement evidence](native-metrics/f8a1af1-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 700 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`68f38bbc1926ae9d3939ed0d4d360fa51ede84e2d5cbc0a842066d980702b8d9`.

## Previous verified commit

Commit `98ce5ea915376f9d52aba173f0eb6e0458f940cf`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`a183d0d2991aae4273174d78dbbe081341d1ad4f502aa9c4f1b280057401e7e0`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,945 | 194 | **50.5446% native** |
| Tests | 24,650 | 76,825 | 0 | **75.7083% Rust** |
| All tracked source, including examples | 76,260 | 129,869 | 194 | 62.9445% native |

The manifests contain 375 production files, 328 test files, and 705 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 66,004 raw
lines minus 13,059 = 52,945; Rust tests are 63,766 separate lines plus 13,059
= 76,825.

Since `f8a1af1`, this pinned cohort retired seven Python test modules and
added 58 named Rust-owned contracts across eight targets. Candidate test
execution now builds the `slop_core` extension from the candidate snapshot for
contracts that import it. Production native share rose 0.0132 percentage
points, and Rust test share rose 1.7620 points. Both independent 50%
source-SLOC milestones remain achieved. The longer-term all-Rust-test target
is still open; these percentages do not measure runtime coverage, behavioral
coverage, or performance. Uncommitted work in the shared checkout is excluded.

The [measurement evidence](native-metrics/98ce5ea-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 705 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`439cf2c93a89270e4d99b43b19886d6652cb4ff1203dbee91a26ef04005614c4`.

## Previous verified commit

Commit `1f4459ceb368eb495068032f61fe6d83bc133509`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`bdbd6acdbe6ef5ec938b172863aac919da12e276ae11201bf9990ab99eb77ff5`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,949 | 194 | **50.5465% native** |
| Tests | 23,449 | 78,795 | 0 | **77.0656% Rust** |
| All tracked source, including examples | 75,059 | 131,843 | 194 | 63.6627% native |

The manifests contain 375 production files, 331 test files, and 708 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 66,008 raw
lines minus 13,059 = 52,949; Rust tests are 65,736 separate lines plus 13,059
= 78,795.

Since `98ce5ea`, this pinned cohort retired five Python test modules and
added 30 named Rust-owned contracts across six targets, including the bounded
Rust subprocess fixture for command-lifetime checks. Production native share
rose 0.0019 percentage points, and Rust test share rose 1.3573 points. Both
independent 50% source-SLOC milestones remain achieved. The longer-term
all-Rust-test target remains open; these percentages do not measure runtime
coverage, behavioral coverage, or performance. Uncommitted work in the shared
checkout is excluded.

The [measurement evidence](native-metrics/1f4459c-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 708 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`5d41abd2e0ec6f7c3a199e2ac53437da0437e7a4db2962b73134093ac3a7334b`.

## Earlier verified commit

Commit `b0d8fced801aa6d85acf6b8e38de9c448a94b736`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`d7d4c3e4c9356e2ac3cd0fa1d1552f40862b0dc23985d3e9bf02d9d2456aca68`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,614 | 53,047 | 204 | **50.5860% native** |
| Tests | 16,894 | 93,560 | 0 | **84.7049% Rust** |
| All tracked source, including examples | 68,508 | 146,706 | 204 | 68.1029% native |

The manifests contain 376 production files, 367 test files, and two Rust
examples, or 745 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 100 test-only ranges in
85 production Rust files: 43,180 original Rust code lines become 30,121 after
masking. The 13,059-line delta belongs to tests. Production Rust is 66,106
raw lines minus 13,059 = 53,047; Rust tests are 80,501 separate lines plus
13,059 = 93,560.

Compared with verified `af62ae0`, this pinned tree retires four executable
Python test suites containing 27 named and 27 statically expanded cases. Four
Rust contract targets add 27 direct tests. Two Python fixture inputs were
added under `src/conductor/testdata/workspace_hooks/`; they are classified as
test data, not executable suites. Production Rust grew by ten code lines and
the native share rose 0.0047 percentage points; Rust test share rose 0.5162
points. The tree retains 42 executable Python test suites with 749 named
tests and 829 statically expanded cases. Its 34 static Python fixture inputs
and shared conftest are inventoried separately. These are source composition
and static inventory measures, not runtime or behavioral coverage claims. The
longer-term all-Rust-test target remains open. Uncommitted checkout work is
excluded.

The [measurement evidence](native-metrics/b0d8fce-evidence.tar.gz) contains
classified path manifests, hashes independently checked against all 745
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`f1bd884dab58d2279c151c6abcfd4ddaf6e07289f79d4e6043f91852d04a27ce`;
the evidence archive SHA-256 is
`ef64081736d7dfab17da2b9b141cdc18e536d0433b45a107a787cdd3cafe7bc1`.

## Earlier verified commit

Commit `af62ae016a705f57e2f0bc7de2b89ef323af14a9`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`4af0cbac05b8ede032eecd299f012322d616ac9255d4c6c2a1619eacea424085`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,614 | 53,037 | 204 | **50.5813% native** |
| Tests | 17,373 | 92,504 | 0 | **84.1887% Rust** |
| All tracked source, including examples | 68,987 | 145,640 | 204 | 67.7928% native |

The manifests contain 376 production files, 365 test files, and two Rust
examples, or 743 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 100 test-only ranges in
85 production Rust files: 43,180 original Rust code lines become 30,121 after
masking. The 13,059-line delta belongs to tests. Production Rust is 66,096
raw lines minus 13,059 = 53,037; Rust tests are 79,445 separate lines plus
13,059 = 92,504.

Relative to `b40b79a`, this pinned tree retires six Python executable test
suites containing 53 named tests and 63 statically expanded parameter cases.
Six Rust contract targets add 51 direct test functions and 12 statically
expanded macro cases, with two shared test helpers and one child fixture.
Production source counts and native share are unchanged; Rust test share rose
0.9296 percentage points. The tree retains 46 executable Python test suites
with 776 named tests and 856 statically expanded parameter cases. Its 32
static Python fixture inputs and shared conftest are inventoried separately.
These are source composition and static inventory measures, not runtime or
behavioral coverage claims. The longer-term all-Rust-test target remains open.
Uncommitted checkout work is excluded.

The [measurement evidence](native-metrics/af62ae0-evidence.tar.gz) contains
classified path manifests, hashes independently checked against all 743
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`d1132f2f5b7e4a9c1ba603bf18596ad054c7a99008d5ad75386ad1f70616f077`;
the evidence archive SHA-256 is
`13997b17d2562cbd6514631666e2784208bb405d8f029a253a6cbb621e1e6392`.

## Earlier verified commit

Commit `b40b79ad42c9188fba9f3cf4ac09e6f637fa0de9`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`c0790c2fc05c078604b8ca93115f9b58ce3f94fb52f06e8bd0fa36f2e210900b`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,614 | 53,037 | 204 | **50.5813% native** |
| Tests | 18,184 | 90,436 | 0 | **83.2591% Rust** |
| All tracked source, including examples | 69,798 | 143,572 | 204 | 67.2235% native |

The manifests contain 376 production files, 362 test files, and two Rust
examples, or 740 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 100 test-only ranges in
85 production Rust files: 43,180 original Rust code lines become 30,121 after
masking. The 13,059-line delta belongs to tests. Production Rust is 66,096
raw lines minus 13,059 = 53,037; Rust tests are 77,377 separate lines plus
13,059 = 90,436.

Relative to `14e570e`, this pinned tree retires the 14-case Python
`test_hook_project_seam.py` suite and adds 15 Rust-owned hook project seam
contracts, including a new executable-precedence case. The production source
counts and native share are unchanged; Rust test share rose 0.3307 percentage
points. The tree retains 52 executable Python test suites with 829 named tests
and 919 statically expanded parameter cases. The 32 static Python fixture
inputs and shared conftest are inventoried separately. These are source
composition and static inventory measures, not runtime or behavioral coverage
claims. The longer-term all-Rust-test target remains open. Uncommitted checkout
work is excluded.

The [measurement evidence](native-metrics/b40b79a-evidence.tar.gz) contains
classified path manifests, hashes independently checked against all 740
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`8648e0a0095c77fb90e9f9492e47a19eb4421bfafa96f5f3155926ad4b2b09c1`;
the evidence archive SHA-256 is
`622af165a244871620f0edb0bd580da8880b7e5bd63a349bad728d72f347d360`.

## Earlier verified commit

Commit `14e570e6c7d2798a1ef9586c0f4e04863bc28a0d`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`cb1caf0292588bce8ba135082f9dcf3988ddc7fdc6e6a76247e658d72d302f63`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,614 | 53,037 | 204 | **50.5813% native** |
| Tests | 18,480 | 89,770 | 0 | **82.9284% Rust** |
| All tracked source, including examples | 70,094 | 142,906 | 204 | 67.0278% native |

The manifests contain 376 production files, 360 test files, and two Rust
examples, or 738 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 100 test-only ranges in
85 production Rust files: 43,180 original Rust code lines become 30,121 after
masking. The 13,059-line delta belongs to tests. Production Rust is 66,096
raw lines minus 13,059 = 53,037; Rust tests are 76,711 separate lines plus
13,059 = 89,770.

Relative to `6ba6818`, the pinned tree retires the 38-case Python
`test_project_init.py` suite and adds 42 Rust-owned project-init contracts,
including four resolver-precedence cases. Native install, prebuild, and
resolver source also changed. Production share rose 0.0324 percentage points
and Rust test share rose 0.5891 points. The tree retains 53 executable Python
test suites with 843 named tests and 933 statically expanded parameter cases.
The 32 static Python fixture inputs and shared conftest are separate from
those suites. This source inventory does not measure runtime coverage,
behavioral coverage, or speed; the longer-term all-Rust-test target is open.
Uncommitted checkout work is excluded.

The [measurement evidence](native-metrics/14e570e-evidence.tar.gz) contains
the classified path manifests, hashes independently checked against all 738
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 85 masked and extracted copies,
tool hashes, the remaining-Python-test inventory, and reproduction scripts.
The SHA-256 of its source-content manifest is
`b5726826902ae632dbc91c9cb75606a902040afe41c4fa3dc1dcb72f813cec23`;
the evidence archive SHA-256 is
`f1cd07846295f59f9a5c993d18f01e6404e30d1723979ccb472466ad0417400a`.

## Earlier verified commit

Commit `6ba68187ffedb4984f32f8f85b0c5a1370efefdc`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`284bc30ed9848c74a1843e15bb029f326b9b1cb4df65286a68c811639a86c1ec`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,954 | 194 | **50.5489% native** |
| Tests | 18,912 | 88,173 | 0 | **82.3393% Rust** |
| All tracked source, including examples | 70,522 | 141,226 | 194 | 66.6343% native |

The manifests contain 375 production files, 354 test files, and 731 counted
source files. Two Rust examples contribute 99 code lines only to the all-source
view. The splitter found 100 test-only ranges in 85 production Rust files:
43,180 original Rust code lines become 30,121 after masking. The 13,059-line
delta belongs to tests. Production Rust is 66,013 raw lines minus 13,059 =
52,954; Rust tests are 75,114 separate lines plus 13,059 = 88,173.

Since `ab49a53`, the pinned cost and attribution cohorts retired six Python
test suites and added 82 named Rust-owned contracts across six targets.
Production source counts and its 50.5489% native share are unchanged; Rust test
share rose 1.3164 percentage points. The pinned tree still contains 54
executable Python test suites, plus 32 static Python fixture inputs and a
shared conftest. These source-composition metrics do not claim behavioral
coverage or completion of the all-Rust-test target. Uncommitted install work
in the shared checkout is excluded.

The [measurement evidence](native-metrics/6ba6818-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 731 counted
Git blobs and a second archive extraction, the repeated archive stream hash,
raw Tokei JSON and independently reproduced per-file reports, inline AST
ranges and verification of all 85 masked and extracted source copies, tool
hashes, and both measurement and verification scripts. Its SHA-256 is
`8b216b7fced07fb7f0d91b107bf24e5fd5c73d919ed7e9e5ecbcc723fd60ce35`.

## Earlier verified commit

Commit `ab49a53a1ec3cef4a21f69370c1fe1ffd25089ba`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`e442af85646f92b2d8e47814d986788a943459e37abfd920b7b4f351d0c13ef8`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,954 | 194 | **50.5489% native** |
| Tests | 20,069 | 85,685 | 0 | **81.0229% Rust** |
| All tracked source, including examples | 71,679 | 138,738 | 194 | 65.8741% native |

The manifests contain 375 production files, 350 test files, and 727 counted
source files. Two Rust examples contribute 99 code lines only to the all-source
view. The splitter found 100 test-only ranges in 85 production Rust files:
43,180 original Rust code lines become 30,121 after masking. The 13,059-line
delta belongs to tests. Production Rust is 66,013 raw lines minus 13,059 =
52,954; Rust tests are 72,626 separate lines plus 13,059 = 85,685.

Since `4111905`, this pinned cohort retired three Python test suites for
baseline merge, protected-delete checks, and commit snapshots, replacing 24
named tests (28 statically expanded cases) with Rust-owned contracts. Production
source counts and its 50.5489% native share are unchanged; Rust test share
rose 0.3356 percentage points. The longer-term all-Rust-test target remains
open: these are source-composition metrics, not behavioral coverage or
performance. Cost and attribution work is excluded from this historical pin.

The [measurement evidence](native-metrics/ab49a53-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 727 counted
Git blobs and a second archive extraction, the repeated archive stream hash,
raw Tokei JSON and independently reproduced per-file reports, inline AST
ranges and verification of all 85 masked and extracted source copies, tool
hashes, and both measurement and verification scripts. Its SHA-256 is
`0b1ca35e7d8beead94b2d5677a25190b5401173d86ca920b8810fd8eda1d40d2`.

## Earlier verified commit

Commit `4111905d2cdf6bca1e5855e6fcd56274c2d92a55`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`50b3b1f4b3a731ee384ac86533e4f2ed46e62d176f8f8be1ca7c9d02f576d270`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,954 | 194 | **50.5489% native** |
| Tests | 20,354 | 85,038 | 0 | **80.6873% Rust** |
| All tracked source, including examples | 71,964 | 138,091 | 194 | 65.6797% native |

The manifests contain 375 production files, 346 test files, and 723 counted
source files. Two Rust examples contribute 99 code lines only to the all-source
view. The splitter found 100 test-only ranges in 85 production Rust files:
43,180 original Rust code lines become 30,121 after masking. The 13,059-line
delta belongs to tests. Production Rust is 66,013 raw lines minus 13,059 =
52,954; Rust tests are 71,979 separate lines plus 13,059 = 85,038.

Since `a344b9a`, this pinned cohort retired three Python test suites for
import-ablation, duplicate-body, and guardrail-AST behavior and replaced the
executable Python dispatcher test stub with a nested Rust fixture crate.
Production source counts and its 50.5489% native share are unchanged. Rust
test share rose 0.7496 percentage points. The longer-term all-Rust-test target
remains open: these are source-composition metrics, not behavioral coverage or
performance. Uncommitted work in the shared checkout is excluded.

The [measurement evidence](native-metrics/4111905-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 723 counted
Git blobs and a second archive extraction, the repeated archive stream hash,
raw Tokei JSON and independently reproduced per-file reports, inline AST
ranges and verification of all 85 masked and extracted source copies, tool
hashes, and both measurement and verification scripts. Its SHA-256 is
`77eb166f8c3e69bcdf20330ff022692764c26f6640ed8192d5b9f9085ef32f49`.

## Earlier verified commit

Commit `a344b9ab8c1b432f2429fd01924b4d6c1bffb688`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`5d8c4525c81da7f3a4c42332d225e331117fb1962bbd0908fc176892e2e405f7`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,954 | 194 | **50.5489% native** |
| Tests | 21,005 | 83,694 | 0 | **79.9377% Rust** |
| All tracked source, including examples | 72,615 | 136,747 | 194 | 65.2556% native |

The manifests contain 375 production files, 343 test files, and 720 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 66,013 raw
lines minus 13,059 = 52,954; Rust tests are 70,635 separate lines plus 13,059
= 83,694.

Since `6dd9fc8`, this pinned cohort retired four Python test modules and
added 39 named Rust-owned contracts across six targets for reuse AST behavior
and the Mull adapter. The original Mull baseline passed 29 cases and skipped
the live-registry case because this checkout registers no Mull campaign; its
Rust replacement checks the registry but an early-return PASS does not claim
that a live campaign was exercised. Production native share rose 0.0014
percentage points, and Rust test share rose 1.7668 points. Both independent
50% source-SLOC milestones remain achieved. The longer-term all-Rust-test
target remains open; these percentages do not measure runtime coverage,
behavioral coverage, or performance. Uncommitted work in the shared checkout
is excluded.

The [measurement evidence](native-metrics/a344b9a-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 720 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`c6d48fee0ba36ba69a181e1eb159d2e5e4c851a5d77429098cc54722d1f2b5a5`.

## Earlier verified commit

Commit `6dd9fc8d31daab114cf856b744ceb20d64cdd7a3`, measured from a
`git archive` with the unchanged path classification, Tokei 15.0.0, and
syn-based inline Rust test treatment. The archive stream SHA-256 is
`e085251f99a80bbf3a6ee0489b4abb79a91e2458d3989c180682cb131f5ad58e`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,610 | 52,951 | 194 | **50.5475% native** |
| Tests | 22,573 | 80,835 | 0 | **78.1709% Rust** |
| All tracked source, including examples | 74,183 | 133,885 | 194 | 64.2868% native |

The manifests contain 375 production files, 336 test files, and 713 counted
source files. Two Rust example files contribute 99 code lines only to the
all-source view. The splitter found 100 test-only ranges in 85 production Rust
files. Those files contain 43,180 original Rust code lines and 30,121 after
masking, assigning a 13,059-line delta to tests. Production Rust is 66,010 raw
lines minus 13,059 = 52,951; Rust tests are 67,776 separate lines plus 13,059
= 80,835.

Since `1f4459c`, this pinned cohort retired three Python test modules and
added 38 named Rust-owned contracts across five targets for reuse audit,
file-family LSH, and generated mutation-engine behavior. Production native
share rose 0.0010 percentage points, and Rust test share rose 1.1053 points.
Both independent 50% source-SLOC milestones remain achieved. The longer-term
all-Rust-test target remains open; these percentages do not measure runtime
coverage, behavioral coverage, or performance. Uncommitted work in the shared
checkout is excluded.

The [measurement evidence](native-metrics/6dd9fc8-evidence.tar.gz) contains
source path manifests and hashes independently checked against all 713 counted
Git blobs, the archive stream hash, raw Tokei JSON and independently
reproduced per-file reports, inline AST ranges and verification of all 85
masked and extracted source copies, tool hashes, and both measurement and
verification scripts. Its SHA-256 is
`1d82d489827993a14d9230849f9ed5220b57ab46940020cf7e42a12d4b49322d`.
