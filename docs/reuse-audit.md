# Code reuse and dead-code audits

Use these advisory commands from the repository you want to inspect:

| Question | Command | Report |
| --- | --- | --- |
| Which functions, files and native implementations could be reused? | `scripts/reuse-audit` | Clone families and native-name evidence |
| Which reviewed consolidation plans remove maintained files? | `scripts/reuse-audit --retirement-plans plans.json` | Signed file-retirement accounting |
| Which Vulture candidates have references elsewhere? | `scripts/dead-code-audit` | Production, test and documentation evidence |

Both launchers live in the Forge checkout. The commands below use their absolute
paths when invoked from another repository. Reports are review evidence; neither
command edits source files or authorizes deletion.

## Reuse inventory

Run a complete inventory from the repository you want to inspect:

```bash
cd /home/tim/Projects/LLM
/home/tim/Projects/llm-forge/scripts/reuse-audit
```

Or choose the repository and report directory explicitly:

```bash
/home/tim/Projects/llm-forge/scripts/reuse-audit \
  --repo /home/tim/Projects/LLM \
  --out-dir /home/tim/Projects/llm-forge/research/reports/reuse-llm/latest
```

The launcher uses the audited repository's `.venv/bin/python`, with the current
Forge source first in `PYTHONPATH`. Set `FORGE_PYTHON` to select a different
interpreter with Forge's declared dependencies. JSCPD must be available through
the audited repository's `node_modules/.bin` or the existing detector discovery
path. Nothing is installed and no refactoring is performed by the command.

For more frequent function checks, add `--quick`. This keeps normalized AST
function clones and the native-name inventory, skips expensive token-block and
whole-file pair analysis, and records that mode in `summary.json`.

## Coverage

The command composes the existing native-backed analyzers:

1. Normalized Python AST function clusters, with a four-line minimum.
2. JSCPD token/block clones, including native source formats, without the default
   maximum file-size/line caps that can hide large files. This mode uses the
   explicit code inputs instead of inherited narrow gate exclusions, maps CUDA
   to the C++ tokenizer, and excludes generated data/configuration formats.
3. Whole-file structural families at similarity 0.70, with up to 10,000 reported
   families and 32 members per family.
4. Existing Python/native name-match candidates, carrying graph freshness and
   completeness separately from the source scans. This stage deliberately checks
   the entire repository graph, including when `--targets` narrows the clone
   scans, so it can find reusable native implementations elsewhere. Its freshness
   check covers that entire graph too.

Source roots are discovered from Git tracked and non-ignored untracked code.
Scanning then includes the live source under those roots. Nested test directories
are explicitly rooted so the detectors' normal test exclusions do not silently
remove test-helper opportunities. `scope.json` records the exact roots,
exclusions and discovered coverage. Use `--targets` for explicit roots or a
non-Git tree; `--exclude` replaces the default excluded path components.
Explicit targets must exist and be directories inside the repository. Invalid
targets fail before reports are written.

Default reports live under `ROOT/.forge/reports/reuse/`. Function and family JSON
retain the full returned candidate lists; Markdown reports provide a readable
entry point. Requested token-scanner failures retain the AST findings, mark the
report incomplete and return nonzero.
Unexplained selected files missing from token-scanner statistics also mark the
report incomplete and return nonzero; the AST findings remain available.
Per-stage logs, commands, timing, fresh report checks and combined status are in
the output directory. An old report is never counted as a successful new run.
The default detector timeout is 900
seconds and can be changed with `--timeout`.
On Linux, a timeout, interrupt, or SIGTERM terminates the detector's process group,
including its Node child, before reporting failure.
The broad token stage defaults to an 8 GiB Node heap bound (`--node-heap-mb`);
an existing `NODE_OPTIONS` is respected. The effective setting is recorded per
stage. This setting affects only that child process. Quick mode does not set it.

`execution_complete` reports whether requested detectors ran successfully;
`source_scans_complete` and `graph_evidence_complete` describe their separate
coverage. Top-level `complete` requires both execution and graph completeness.
A dirty graph can make `complete` false while the successfully executed command
returns zero. Detector execution failures or incomplete source scans return 2.

## Review before consolidating

All candidates are advisory. Exact-looking function bodies may resolve globals
from different imports, use different decorators, or expose keyword arguments
whose names are API contracts. Structural similarity between novel mechanisms
and controls is not permission to merge them. Historical task snapshots and test
scaffolding should be reviewed separately from current library code.

Existing fields named `tokens` and `est_bytes` measure AST structural units,
not actual bytes saved. Family LOC savings are estimates. Native name matches do
not show equivalent behavior or a performance gain. Dirty/stale graph evidence
remains explicitly incomplete even when the source clone scanners succeed.

## Rank explicit file retirements

Add `--retirement-plans /path/to/plans.json` to either the sweep command or
`python -m conductor.reuse.file_families`. Similarity alone never establishes
that a file can be retired. Without explicit plans, `net_files_removed` remains
unknown and the existing detector ranking is unchanged.
The sweep rejects a plan input that aliases any report or log it will write,
including through symlinks or hard links. A separate input filename inside the
output directory is allowed.

```json
{
  "schema_version": 1,
  "plans": [{
    "id": "combine-lane-specs",
    "status": "reviewed",
    "reviewer": "reviewer-name",
    "remove": ["pkg/compression_adapter.py", "pkg/router_adapter.py"],
    "retain": [],
    "add": ["pkg/lane_specs.py"],
    "add_adapters": [],
    "add_configs": [],
    "evidence_ids": ["functions:C012", "families:F003"],
    "callers": ["pkg/proposer.py", "tests/test_lanes.py"],
    "semantic_constraints": ["Preserve each lane's schema, parser and checker"],
    "provenance": ["Source revision and review record identifying the exact files"]
  }]
}
```

Every path is relative to the audited repository. `remove`, `retain`, `add`,
`add_adapters`, and `add_configs` describe the complete maintained-file footprint
of the proposal. List existing adapters/configs under `retain`; list every newly
maintained file in one of the added lists. Duplicate paths across added lists
count once. `callers` records files requiring integration review, without counting
existing callers as new files. Nothing in the plan is executed or deleted.

Net file retirement is unique removed paths minus the union of all added paths.
Two old modules replaced by one module score **1**; seven originals retained
plus a new base module score **−1**. A new config file or compatibility adapter
reduces the saving. Removed, retained and added sets must be disjoint. Plans with
overlapping maintained files must be combined into one plan, including all their
evidence IDs, so totals cannot count the same retirement twice.

Positive `reviewed` plans rank first, then positive `planned` plans, each by net
file reduction. Review status is a user declaration, not an authorization receipt.
Reviewed plans require a reviewer, semantic constraints and provenance. Every
result is labeled `measurement_status: "not_measured"`; `measured` is not an
accepted input status. File existence, caller compatibility, historical receipt
bindings and numerical equivalence still require review before implementation.

Explicit plans remain in the report even if LOC increases or detector thresholds
and caps omit their evidence. Matching family rows rank by their associated plans
without renumbering detector IDs; unresolved families retain their previous order.
`families.json` and its Markdown contain the plans and the original LOC estimates.
The sweep also writes `retirement-plans.json`, merging fresh function/family evidence
under each plan once, and `retirement-plan-input.json`, the validated input snapshot
used by its child detector. Quick mode reports plans with function evidence only.
Declared evidence IDs remain provenance even when their original report is absent;
they are not automatically verified against the current scan. Persist the original
report/revision in `provenance`, since IDs can differ between scans.

Prefer reusing an existing canonical helper. When a new shared responsibility is
needed, make behavior differences explicit rather than hiding them in broad
fallbacks. Rust is a good candidate for measured repeated parsing, normalization,
scoring and bulk processing; keep small I/O/CLI glue thin. Hashes, persisted
receipts, numerical behavior and scientific controls require parity tests before
changing implementation or serialization.

## Dead-code reference audit

`scripts/dead-code-audit` runs Vulture, then uses the existing Rust Python AST
parser to classify each candidate using references across the repository. It is
the durable port of the `vast.py` scratch classifier. Vulture remains the Python
candidate detector; indexing, classification and report generation run in Rust.

For Component Fab:

```bash
cd /home/tim/Projects/LLM
/home/tim/Projects/llm-forge/scripts/dead-code-audit \
  --path component_fab \
  --whitelist research/tools/vulture_whitelist.py
```

The first invocation builds the release binary with the repository's Rust 1.98.0
toolchain. Subsequent invocations reuse it. It does not install packages into the
project's environment or schedule background jobs. Vulture is already declared
in Forge's runtime dependencies; by default the executable is
`ROOT/.venv/bin/vulture`. Select another with `--vulture /absolute/path/to/vulture`.
Run `scripts/dead-code-audit --help` for all options.

The default report is `ROOT/.forge/reports/dead-code/latest.json`. Each completed
scan replaces it atomically. The report directory, selected output, whitelists and
replayed findings are excluded from reference matching, so reports cannot keep
their own candidates alive. When saving additional reports elsewhere inside the
repository, exclude their directory with `--exclude-reference PATH`.

### Meaning of the report

The JSON object contains `complete`, `counts`, `findings`, `diagnostics`, scanned
file counts and invocation metadata. Each finding retains the original
`file`, `line`, `kind`, `name`, `conf`, `lines`, and `size`, plus:

| Verdict | Evidence |
| --- | --- |
| `ALIVE` | At least one reference in production code |
| `TEST-ONLY` | References in tests, but none in production code |
| `DOC-ONLY` | Documentation mentions, but no code references |
| `DEAD` | No references found outside the finding's own file |

`prod`, `tests` and `docs` contain complete `[path, line, kind]` reference lists.
They are no longer truncated to 8/8/3 entries. A test path has a `tests` component
or a filename starting with `test_`. Markdown and text files supply documentation
mentions. Other supported source/config formats supply textual code references.

Python references include names, terminal attributes, definitions, original import
names and aliases, and decoded string literals matched on identifier boundaries.
For example, `module.symbol`, `"package:symbol"`, and
`from package import symbol as alias` retain the evidence missed by the older
name-only scratch scripts. Comments alone are not Python AST references.

This retains the original **bare-name evidence** semantics, not qualified symbol
resolution: an unrelated same-named definition, an assignment, or a docstring can
count as code evidence. All same-file references are excluded. `ALIVE` does not
prove runtime reachability; `DEAD` is a review candidate, not permission to delete
code. Dynamic registration, public APIs, schema fields and novel research lanes
still require review. The original `vast.py` did not perform decorator/registry
adjudication; that was a separate scratch script, and no registration-liveness
inference is introduced here.

Explicit shell shebangs are recognized even on a file named `.py`; these files
supply text references. Build/cache trees (`target` and `.cache`) are pruned in
addition to the scratch script's exclusions.

Unreadable files and Python syntax failures make the report incomplete and return
exit status 2. Execution errors or malformed Vulture output do not replace the
last report. A complete report returns 0 regardless of the number of candidates.
Explicit exclusions are recorded; do not use them to hide unexplained failures.
The selected output must be a `.json` file and must not collide with a replay,
whitelist, or explicitly selected source file. Vulture currently has no internal
timeout; use an external process supervisor if scheduling this command.

### Reproduce and test

Replay the same findings without running Vulture again:

```bash
scripts/dead-code-audit --root /path/to/repository \
  --vulture-output /tmp/vulture.stdout.txt --output /tmp/adjudicated.json
```

The replay input must be plain Vulture stdout, including optional size suffixes
from `--sort-by-size`. It is not the JSON report. The report records the replay
path instead of inventing a Vulture execution status.

```bash
cargo +1.98.0 test --locked \
  --manifest-path native/conductor-native/Cargo.toml \
  --target-dir native/conductor-native/target/variants/source-analysis --jobs 2 \
  --no-default-features --features source-analysis --lib dead_code
cargo +1.98.0 test --locked \
  --manifest-path native/conductor-native/Cargo.toml \
  --target-dir native/conductor-native/target/variants/source-analysis --jobs 2 \
  --no-default-features --features source-analysis --test dead_code_cli
```

Forge already has a Vulture gate adapter, a reuse inventory and a test-reference
index. Those collect candidates or test dependencies; they do not produce this
production/test/documentation evidence table. The adjudicator reuses the existing
native parser dependencies and remains a separate advisory command, without
changing any existing gate.

### Validation on LLM

The initial port matched all 410 finding identities/verdicts and all 37,959
reference occurrences from the uncapped Python classifier (reference ordering
ignored). The 12 native regressions and 7 CLI integration tests passed, as did
strict Clippy for the library and binary.

With identical captured Vulture findings, sequential warm classification runs
took 32.30 seconds in Python and 0.88 seconds in release Rust. A live Rust run
including Vulture took 2.10 seconds. These are single-run observations on the
same live checkout, not a statistical performance guarantee. Local validation
receipts and full parity inputs are saved under
`research/reports/dead-code/validation/`; the live audit is
`research/reports/dead-code/llm.json` in this Forge checkout.
