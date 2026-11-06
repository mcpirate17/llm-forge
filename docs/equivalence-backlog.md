# Native equivalence backlog

`forge equivalence backlog` folds existing equivalence summaries into a ledger
and Markdown report. It reuses Slop Core's Rust fold and launches no interpreter,
model, test driver or equivalence probe. A partial or incomplete run preserves
previous findings outside the scope it actually measured.

```sh
# Output parent directories must already exist.
forge equivalence backlog summary.json --root /path/to/host
forge equivalence backlog summary.json --root /path/to/host --apply
forge equivalence backlog --root /path/to/host --apply --consume-recorded --json
```

Preview is the default; `--dry-run` is an explicit equivalent. It renders the
proposed report, writes nothing and consumes no inputs. `--apply` publishes the
report and ledger. `--consume-recorded` requires `--apply` and removes only the
unchanged, captured `gate-*.json` files discovered in `run_artifacts`, after both
outputs have been published and synced. An explicitly supplied summary is kept
unless it was also discovered there. Files arriving after discovery are kept for
the next invocation. `--json` emits schema version 1, counts, output paths and
consumed paths; preview also includes the proposed report.

The host is the nearest ancestor of `--root` declaring `[tool.forge.test_record]`.
Without that table, the supplied root (default current directory) is used.
Configuration remains in `pyproject.toml`:

```toml
[tool.forge.equivalence_probe]
ledger = "reports/equivalence-ledger.json"
report = "reports/equivalence-backlog.md"
run_artifacts = "reports/equivalence-runs"
```

Explicit summaries are processed in argument order, followed by sorted discovered
artifacts; duplicates preserve the existing fold semantics. Configured paths are
host-relative, absolute, or `~/`-rooted. Explicit `--ledger` and `--report` paths
are relative to the invocation directory and authorize their selected destination.
Configured outputs must stay within the host; use explicit flags to select
external outputs. Consuming configured artifacts outside the host is refused.
The unused probe keys are still type-checked, so configuration mistakes fail loudly.

The native boundary requires existing, safely owned output parents and normalized
paths. It refuses symlinks, hard-linked inputs/outputs, traversal, world-writable
inputs, output/input aliases, duplicate summary JSON keys and output names that could be
rediscovered as `gate-*.json`. Named-user `~user` expansion is unsupported; supply
an absolute path. Missing or whitespace-only ledgers remain empty ledgers.
Read-only observation permits group-writable repository data. Publication and
consumption require parents without group/world write permissions and existing
affected files owned by the operator without group/world write permissions.
These checks run before staging and again before replacement or removal.
Limits are 256 summaries, 16 MiB per summary/output/previous ledger or report,
64 MiB total summary bytes, 4,096 artifact-directory entries, and 64 ancestor
configuration files of at most 1 MiB each. Summary JSON nesting is bounded to 32 levels.

Both outputs are staged before either is replaced. Cooperating publishers lock
both parent directories in a consistent order and recheck captured inputs before
replacement. The report is replaced first, then the ledger. **Two replacements
are not a transaction**: an error identifies which output was renamed and synced;
artifacts are retained on publication failure. Consumption errors identify the
removed and remaining files. A sync error can follow an actual replacement or
removal and is reported explicitly. Hostile concurrent writes by the same Unix
user remain outside the cooperative-parent guarantee. Backlog artifacts are
observed input data, not authenticated evidence or execution authority.
Path ancestors must also remain trusted and stable; these checks are not a
general filesystem sandbox against hostile ancestor replacement.

`make slop-backlog` still invokes the Python equivalence gate first, then calls
`$(FORGE) equivalence backlog --apply --consume-recorded` for the native fold.
Set `FORGE` to the intended executable. The old Python/PyO3 backlog entry point
remains a compatibility adapter using the same Rust fold. Both artifacts now use
one clock instant, instead of independent report and ledger timestamps. The native
preview default, explicit consumption and path restrictions are intentional
changes from that legacy adapter. No whole-gate Rust migration is implied.

Focused tests cover publication failures, stale inputs, partial measurement,
configuration, cleanup and actual CLI execution with an empty `PATH`. The Make
handoff test uses an authored gate stub; it does not run a real probe. Current
comparison and release evidence is indexed in the
[engineering report](../research/performance/forge-program-20261106-backlog.json).
