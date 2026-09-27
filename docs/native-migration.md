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

Commit `e246a964d481215beede5c9cf2cb9531804aa9e0`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`5611106324c84fae281b3a76d9a1e65ad61f40f89da0f9215e4019d707c8deec`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,633 | 53,774 | 204 | **50.9170% native** |
| Tests | 5,578 | 116,677 | 0 | **95.4374% Rust** |
| All tracked source, including examples | 57,211 | 170,550 | 204 | 74.8141% native |

The manifests contain 378 production files, 397 test files, and two Rust
examples, or 777 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 102 test-only ranges in
87 production Rust files: 43,893 original code lines become 30,789 after
masking. The 13,104-line delta belongs to tests. Production Rust is 66,878
raw lines minus 13,104 = 53,774; Rust tests are 103,573 separate lines plus
13,104 = 116,677.

Compared with verified `eeb30d5`, production native share rose 0.0218
percentage points and Rust test share rose 3.0024 points. This 0.1.69 native
cohort retired six Python test suites with 193 named and 195 statically
expanded cases: CPU embedding (54), equivalence probe (34), mutation campaign
generation (26), mutation retention (32), workspace hygiene (17), and
worktree reaping (32). Their Rust contracts contain 195 corresponding cases.
Two additional Rust cases cover the hook exposure corpus; its two Python
cases remain active. The pinned tree retains six executable Python suites
with 122 named and 142 statically expanded cases: candidate review (56),
call and evidence (39), hardening (12), scan coverage (6), mutation patch
audit (27), and hook exposure (2). It also retains 34 earlier Python fixture
inputs and shared `conftest.py`. Seven new equivalence-probe Python input
fixtures are included in Python test SLOC; they are not Rust tests.

The [CPU embedding map](native-cpu-embed-contract-migration.md),
[probe map](native-equivalence-probe-contract-migration.md),
[mutation map](native-mutation-tools-contract-migration.md), and
[workspace map](native-workspace-corpus-contract-migration.md) record all 197
Rust case correspondences. Scoped validation passed 197/197 contract cases,
32/32 discovery cases, 15/15 workspace parity cases, and Clippy. These logs
do not replace the required full local check and verification gate, pending
when this evidence was assembled. The last recorded full gate was on earlier
`415ae7c`, not this source. No matched performance comparison ran for this
cohort. The prior 134-case paired harness benchmark, at `8d71bca` in
[history](native-migration-history.md), found 1.53 s Python versus 2.35 s
Rust, 53.6% slower, with 16.5% lower Rust peak RSS; it is not a measurement
of this cohort or production throughput.

The [measurement evidence](native-metrics/e246a96-evidence.tar.gz) contains
classified manifests and hashes independently checked against all 777 counted
Git blobs and a second archive extraction, per-file Tokei reports, inline AST
ranges and all 87 masked copies, the Python retirement inventory, case map,
cohort source hashes, reproduction scripts, and scoped validation logs. The
SHA-256 of its source-content manifest is
`9fa38c9c6200bea79bd63c5309c782f9392525bf84cb83af6ac5cdf1dcfb0a31`;
the evidence archive SHA-256 is
`d4c8817dbac293b026c72064c88afbf1ed2a0546cfff746f5329a559fb27ed19`.

## Previous verified commit

Commit `eeb30d5dd2c7956918d0b62067d002faa8ec10d9`, measured from a
plain `git archive` with the unchanged path classification, Tokei 15.0.0,
and syn-based inline Rust test treatment. The archive stream SHA-256 is
`c88c126723ae6ecc9400b55f0af7b0a78fbf5c23940041bda4969eb85e10f184`.

| Scope | Python code lines | Rust code lines | Shell code lines | Native / Rust share |
| --- | ---: | ---: | ---: | ---: |
| Production | 51,633 | 53,727 | 204 | **50.8952% native** |
| Tests | 8,985 | 109,785 | 0 | **92.4350% Rust** |
| All tracked source, including examples | 60,618 | 163,611 | 204 | 72.8997% native |

The manifests contain 378 production files, 377 test files, and two Rust
examples, or 757 counted source files. The examples contribute 99 Rust code
lines only to the all-source view. The splitter found 102 test-only ranges in
87 production Rust files: 43,893 original code lines become 30,789 after
masking. The 13,104-line delta belongs to tests. Production Rust is 66,831
raw lines minus 13,104 = 53,727; Rust tests are 96,681 separate lines plus
13,104 = 109,785.

Compared with verified `8d71bca`, production native share is unchanged and
Rust test share rose 1.6266 percentage points. This cohort retired five
executable Python suites with 93 named and 124 statically expanded cases:
doctor (34), package resources (23), installed layout (one), tooling boundary
(40), and mutation attribution (26). Five Rust contract targets contain 124
corresponding tests. The pinned tree retains 12 executable Python test suites
with 315 named and 337 statically expanded cases, plus 34 Python fixture
inputs and shared `conftest.py`. In the current Forge environment, 303 of
those cases collect. The 34-case `test_equivalence_probe.py` module calls
`pytest.importorskip("torch")`, and torch is absent; its static cases remain
counted, but they did not execute. Collection is not a test pass.

Scoped validation passed 34 doctor, 23 resource, one installed-layout, 40
tooling-boundary, and 26 attribution Rust cases at their recorded stages.
The resource and installed-layout runs followed the fixture repair; the
40-case tooling-boundary run preceded it. The committed 0.1.68 source also
passed 29 discovery and 26 attribution integration cases and scoped Clippy.
The required full local check and verification gate was pending when this
evidence was assembled. There was no matched performance experiment for this
cohort. The resource target's 3.00-second scoped duration does not establish a
speed difference; the prior 134-case matched benchmark remains tied to
`8d71bca` in the migration history.

The [measurement evidence](native-metrics/eeb30d5-evidence.tar.gz) contains
classified path manifests and hashes independently checked against all 757
counted Git blobs and a second archive extraction, per-file Tokei reports,
inline AST ranges and verification of all 87 masked and extracted copies,
tool hashes, the five-suite Python retirement inventory and exact 124-case
Rust map, reproduction scripts, and phase-labeled scoped validation logs.
The SHA-256 of its source-content manifest is
`9c7e3b3abc0c5edfdd8c0b4a78c6ab0924ef6733455cf3d08210f93153293a6e`;
the evidence archive SHA-256 is
`259c9310b9d6e2173a101c1217e44d0e5603b2b7261c024615521fa27e875b7f`.

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
