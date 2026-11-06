# Native source analysis

`forge analyze` exposes the existing Rust scanners directly. Its default build
links `slop-core` without PyO3, interpreter discovery or subprocess scanners.
Source text may be Python; running the analysis never executes it.

```sh
forge analyze rules
forge analyze --root /path/to/repo scan src/model.py native/core.rs
forge analyze --root /path/to/repo scan src/model.py --checks fallback,style
forge analyze --root /path/to/repo scan models/model.py --checks hot-loop --hot-path models
forge analyze --root /path/to/repo ablations src/model.py --rule drop_clamp --qualname Model.forward
```

Scanning returns JSON with each source's SHA-256, the checks actually applied,
ordered findings, the complete finding count, and an explicit truncation flag.
Exit status is 0 for a complete scan without findings, 1 for findings, and 2 for
invalid input or tool failure. All files must succeed before any JSON is printed.
Missing, unreadable or malformed source never produces a clean result.

The available check families are:

| Check | Source | Scope |
| --- | --- | --- |
| `rust` | Rust | Production unwrap calls and stub macros, with existing test-scope exemptions |
| `style` | Python | Comment patterns, constant conditions, unreachable statements, empty functions and configuration literals |
| `fallback` | Python | Silent exception handlers, retaining explicit in-handler waivers |
| `test-value` | Python | Constant assertions and test functions with no assertion |
| `verbosity` | Python | Functions with disproportionate own-line comments |
| `hot-loop` | Python | Tensor/array loop heuristics within explicitly selected hot paths |

Omitting `--checks` runs every applicable family. Hot-loop analysis requires a
matching `--hot-path`; no host directory is presumed performance-sensitive.
Explicitly requesting checks that cannot run on a file is an error. Duplicate
file arguments are scanned once. Findings are static heuristics, not proof of
reachability, test effectiveness, dynamic types or measured performance.

Python style/ablation analysis and Rust checks use the shared Tree-sitter 0.27
ABI. Combined Python checks share one immutable source and one parse by each
native parser; the source cannot be swapped independently of its ASTs. The
fallback/structural detector uses Ruff's native Python AST. A fallback-only
request keeps its smaller Ruff-only path. Its previous
CPython traversal has been deleted. Exception names, attributes and tuples have
stable formatting; other exception expressions retain their original spelling.
Structural findings use source order. The low-level audit API reports unparsed
files as explicit coverage gaps while retaining line-based file findings; the
CLI's gate scan fails on invalid syntax.

## Selected-source bounds and file handling

The CLI uses the same source-directory capability as `forge code`: relative
paths, no final symlinks or special files, strict UTF-8, a 1 MiB per-file bound,
and metadata stability checks across reads. It accepts at most 256 files and
16 MiB of source per invocation. Up to 64 explicit hot paths are allowed.
Returned findings/proposals are capped by `--max-results` (1–4096, default 500
for scans and 100 for ablations). The serialized output must fit `--max-bytes`
(512 bytes–4 MiB, default 64 KiB), or the operation fails before printing it.
Parser and individual scanner allocations remain proportional to the bounded
source; these scanners do not claim zero allocation or a hard execution deadline.

## Python test import report

```sh
forge analyze --root /path/to/repo tests
forge analyze --root /path/to/repo --max-bytes 4194304 tests --check
forge analyze --root /path/to/repo tests --json-out tasks/audit/dead_tests.json
make dead-tests
```

`tests` reuses the shared Rust dead-test classifier. Native Git inventory, history,
reference collection and parsing require neither a Git executable nor Python on
`PATH`; no source imports or tests are executed. The scope is tracked Python
modules and Python test filenames, not Rust test coverage or dynamic reachability.
All findings are advisory and provide no authority to delete code or tests.

The versioned JSON envelope includes the canonical repository root, HEAD,
shallow-history flag, source/lookup digest summaries and the complete `report`:

| Signal | Meaning |
| --- | --- |
| `broken` | A test's transitive first-party closure contains a hard missing import |
| `depends_on_untracked`, `untracked_importers` | Present but untracked first-party dependencies and their importers |
| `stale_imports` | Missing first-party imports in guarded or deferred code |
| `orphan_target` | Direct test targets have no detected non-test importer or configuration reference |
| `unreferenced_sources` | Non-test modules have no detected importer or reference |
| `untracked_tests` | Untracked Python test files, respecting Git ignores |

Broken findings take precedence over untracked/orphan rows for the same test.
Configuration references suppress orphan/unreferenced signals; `notes_only`
annotations do not suppress them. Reference text is a heuristic, not proof of
execution. History reports the first encountered non-merge change's author-local
day, follows both merge parents, and treats shallow boundaries as roots. Shallow
history cannot establish dates beyond the locally available boundary.

The default prints JSON and writes no artifact. `--json-out` explicitly publishes
the legacy report object, without its envelope, using the existing atomic writer.
It must name an untracked `.json` file inside the repository and outside captured
sources, reference trees and Git metadata. `make dead-tests` selects
`$(CONDUCTOR_AUDIT_DIR)/dead_tests.json` and prints the native envelope. Exit 0 means
the report completed; `--check` returns 1 only for `broken` findings. Unsafe,
malformed, changed or unsupported input and output overflow return 2 without
publishing a new report. A previous artifact is retained on failure and must not
be mistaken for that failed invocation's result.

The input capture admits at most 100,000 files, 16 MiB per source and 1 GiB total;
each reference corpus is limited to 32 MiB. Git index admission is 32 MiB. History
has 100,000-commit/path limits, one million tree-entry/path-event limits, 128 MiB
metadata admission, bounded relevant rename work (65,536 pairs), and a cooperative
30-second history deadline. These are not a hard whole-command time/RSS limit:
libgit2 discovery/status and native parsing have additional work. Oversized or
unsupported history refuses rather than silently dropping dates or rename matches.
Rename scoring is skipped when no added/deleted path can affect a currently tracked
Python path's date. Relevant diffs retain all competing candidates; full history,
tree and path admission still run. This report does not verify unrelated blob contents.
The ordinary `--max-bytes` output ceiling applies; no partial report is emitted.

Source bytes, import existence observations, Git identities/settings/inventories
and reference inventories are rechecked before publication. These are cooperative
drift checks, not an atomic filesystem snapshot or protection against a hostile
same-user process. Selected symlinks and nonregular files are refused. Reference
text and Python parsing retain the classifier's lossy UTF-8 compatibility; project
configuration must be valid UTF-8. Unsafe/non-UTF-8 Git paths are refused.

Two deliberate corrections differ from the old Python collector: mutation
campaign references are excluded using the configured registry parent, and Unicode
path dates use raw Git paths instead of text-quoted keys. Git replacements/grafts,
inline configuration overrides and environment-based repository redirection are
unsupported and refuse explicitly. `GIT_INDEX_FILE` and supported
`GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM` selections are honored; an explicit global
file together with an existing XDG Git config refuses because the pinned libgit2
would otherwise combine files that Git treats differently.

The Python `conductor.dead_tests` API remains for host compatibility and delegates
classification to the same Rust implementation. Its legacy collector and human
summary remain distinct. The optional operator-enabled `python_test_report`
harness tool exposes the same advisory envelope for the exact configured root,
without artifact publication; see [the capability and cancellation
boundary](native-harness.md#optional-python-test-report). Its isolated worker
has a child deadline; filesystem setup and verification remain cooperative.
This interface does not establish use by an external coding client.

## Ablation proposals

`ablations` returns automatic rule-generated byte-span edits and the source hash.
`--expected-hash` refuses source drift, `--rule` restricts the registry,
`--extra` adds optional rules, and `--qualname` restricts the target function.
Unknown rule names fail; duplicate rule names cannot duplicate proposals.
The input AST is limited to 65,536 nodes and depth 128. Functions accept at most
64 parameters; the sum of parameter count times function subtree size is capped
at one million. Default expressions are limited to 4 KiB, a parameter expansion
to 2 MiB of edits, and a collection to 4,096 proposals / 8 MiB of retained
proposal data. Exceeding these limits fails the operation. Every returned proposal
is applied to an in-memory copy and validated by both parsers before emission.
The old unchecked edit function has been removed. Ruff validation caught and
helped fix a binding-site rewrite regression after the Tree-sitter upgrade,
including destructured `with ... as` targets. Invalid UTF-8
boundaries, overlapping/ambiguous edits, oversized output and malformed syntax
are errors. The source file is never written, and the response records
`executed: false`. Unreturned candidates are counted but not individually
validated when a result limit truncates the output.

Proposals are not mutation-test evidence. Campaign execution and receipts remain
the responsibility of the automatic engines in disposable snapshots. No
ablation removes or replaces a mechanism in a production checkout.

## Migration boundary and validation

The default `slop-core` dependency closure contains no Python binding. Its
native scanners, ablation engine, test index, structural candidate formatting,
file-family comparison, report rendering and backlog algorithms remain available
as Rust APIs. The existing CPython adapters require an explicit `python` feature;
consolidation/file profiling and the old interpreter-driven equivalence runner
are still confined to that transitional feature and require further migration.
They are not included in Forge's executable.

After the native detector and checked proposal API changes, 181 release tests
and strict all-target Clippy passed with `PYO3_PYTHON` pointing at a nonexistent
executable. The separate transitional binding build is checked explicitly. The 181 tests
include existing scanner/rule behavior and new parser, bounds and checked-edit
contracts; they are not a repository-wide release receipt.


## Measured parsing cost

Rust 1.98 release builds on an AMD Ryzen 9 9950X3D, shared Linux host, five warmups
and 31 batches of ten operations. These are batch-mean medians and p95 values,
not individual-request latency percentiles. The 21,778-byte Python fixture has
128 functions and SHA-256
`9904181637b0b9f0c1dcc128382ae3b71d64ba22c916235aadec5e62ab60eeab`.

| Workload | Median | p95 | Result |
| --- | ---: | ---: | --- |
| Fallback scan | 282.5 µs | 300.7 µs | 128 findings |
| Five combined check families | 4.962 ms | 4.978 ms | 256 findings |
| Checked ablation collection | 4.117 ms | 4.133 ms | 128 candidates |
| Checked apply, 36-byte source | 11.58 µs | 11.85 µs | 23-byte candidate |

Before sharing parsed sources, the combined checks took 8.710 ms median and
8.733 ms p95 on the same fixture and returned the same 256 findings. The observed
median reduction was 43%; this compares native implementations on synthetic
source, without a Python baseline or a claim about arbitrary repositories.
The stronger validation and expansion limits increased ablation collection
from 3.662 to 4.117 ms and small checked application from 9.20 to 11.58 µs.

The before benchmark executable SHA-256 was
`75a8f8ccadb60a811d73be039505056b8c0d59c5cc79626155584d5c903bb570`;
the after executable was
`9f2aac48458f9da884af23402c0cc5d50e9b21f4d5b5a4e91713472b620b89a7`.
Reproduce with `cargo +1.98.0 bench --locked --manifest-path
native/slop-core/Cargo.toml --bench analysis`. This measures parsing and checking,
excluding CLI startup, file reads, model latency and serialization.


The subsequent integrated release sweep passed 554 tests with one existing
ignored test, including nine empty-PATH analysis CLI tests, native search,
installation/doctor, SessionStart, the Forge binary and both frozen hook corpora.
Strict all-target Clippy passed for both crates. The current default Forge
runtime dependency graph contains no PyO3 or libpython; Rust crates that parse
Python source are ordinary native dependencies.
