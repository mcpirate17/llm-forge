# Native migration measurement history archive

Earlier source-pinned measurements moved here to keep the primary history concise. The overview and current history are in [native migration](native-migration.md) and [its history](native-migration-history.md).

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
