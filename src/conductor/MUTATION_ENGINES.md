# Generated mutation engines

## Current execution policy

**Mutate only what you changed** -- the files this branch modified and the tests
that exercise them, never the whole repository. `mutation_campaign_generate`
scopes itself to `git diff <base>...HEAD` plus the invoking owner's claimed
dirty files. Repeated `--only` selectors narrow the task scope. `--all-files`
is an inventory/planning option, not authorization to execute a whole-tree run.
The runner independently refuses targets outside the agent's changed files.

Only conductor-managed, engine-generated mutation campaigns may execute. Agents
must not author, apply, score, re-pin, or run patch-based mutants manually. The
hand runner and its `repin` driver were deleted on 2026-09-08; legacy patch
manifests and receipts remain archival provenance only, never an executable
testing path.

Mutation testing asks one question: **if I introduce a realistic bug, does any
test notice?** The score is the share of introduced bugs the suite caught.

## Why this replaced the old system

The historic 489 campaigns under `conductor/mutation_campaigns/` declare
`mutation_engine: reviewed_unified_diff`. An agent chose each bug, wrote it as a
committed patch, and then scored the suite on catching the bugs it had chosen.
**482 of 483 scored campaigns publish exactly 1.0.** A mechanical sweep of the
same code kills 60.6%.

A corpus an agent selects measures the agent's selection, not the tests. That is
the gameable part, and no amount of review fixes it, because the reviewer is
looking at the mutants that were offered rather than the ones that were not.

A generated engine takes the mutants from the source tree. Every `<` becomes
`<=`, every `!` is deleted, every return value is replaced -- exhaustively, by a
third-party tool with no knowledge of the campaign, the tests, or who is being
graded. Nobody chooses. There is nothing to curate, so there is nothing to game.

The number that matters also changes. A percentage invites tuning, so a generated
campaign is scored on its **survivor set**: the exact mutants that lived. It
passes when no mutant survives that did not survive the recorded baseline. One
new survivor, red campaign -- regardless of what the percentage did.

## The three engines

| Language | Engine | Version | Mutates |
|---|---|---|---|
| Python | [fest](https://github.com/sakost/fest) | 0.1.3 | source text, 12 operators |
| Rust | [cargo-mutants](https://github.com/sourcefrog/cargo-mutants) | 27.1.0 | the parsed AST |
| C / C++ | [Mull](https://github.com/mull-project/mull) | 0.34.0 (LLVM 18) | LLVM IR, at compile time |

Installation:

- **fest** is declared in `pyproject.toml` as `fest-mutate`, so `uv sync`
  provides it. A shipped module's dependency never lives only in a CI job.
- **cargo-mutants**: `cargo install cargo-mutants`. A developer tool, not a
  shipped dependency, so it belongs to the toolchain rather than to `pyproject`.
- **Mull**: `mull-18` from the project's apt repository, which supplies both
  `/usr/bin/mull-runner-18` and the clang pass plugin
  `/usr/lib/mull-ir-frontend-18`. Needs a matching `clang-18` and
  `llvm-profdata-18`.

## The common interface

Three tools with nothing in common -- one reads text, one reads an AST, one
rewrites compiler IR; one exits 2 when a mutant survives, another exits 0 while
reporting that nothing ran. They meet at four shared things:

1. **One manifest schema.** `conductor/mutation_campaigns/<id>.json` declares the
   subject, the test command, the bounds and the survivor baseline.
2. **One CLI.** `python -m conductor.mutation_engine_generated`; the adapter is
   chosen from the manifest's `mutation_engine`.
3. **One receipt schema.** `llm.mutation-testing.receipt.v3`, with one outcome
   vocabulary across all three engines.
4. **One scoring rule.** The survivor set, compared against the baseline.

An engine adapter supplies exactly two things: where its binary is, and how to
turn one run into receipt rows. `mutation_engine_fest.py`,
`mutation_engine_cargo.py` and `mutation_engine_mull.py` are 276, 300 and 572
lines respectively; everything else is shared in `mutation_engine_generated.py`.

### Outcomes

| Outcome | Meaning |
|---|---|
| `KILLED` | a test failed. The suite caught it. |
| `SURVIVED` | every test passed. The suite would ship this bug. |
| `NO_COVERAGE` | no test reaches this line at all. |
| `UNVIABLE` | the mutant did not compile. |
| `TIMED_OUT` | the mutant ran past its budget, usually an infinite loop. |
| `ERROR` | the run itself failed. |

**Only `KILLED` and `SURVIVED` score.** The denominator is `KILLED + SURVIVED`.
A mutant that never compiled or was never reached was never a test of anything,
and folding it into the caught count -- which any plain kill-fraction does --
credits the suite with detections it never made. cargo-mutants reported 24
unviable mutants on the first crate measured.

### Statuses and exit codes

| Status | Exit | Meaning |
|---|---|---|
| `PASS` | 0 | no survivors at all |
| `RATCHET_HELD` | 0 | survivors, all of them in the recorded baseline |
| `FAIL` | 1 | at least one survivor that is not in the baseline |
| `ERROR` | 1 | the run did not complete |
| `NOT_READY` | 3 | `inspect` found the campaign unrunnable |
| `REFUSED` | 4 | the manifest or the request was rejected |

## Using it

Generate a campaign per subject -- one Python module with the tests that name it,
or exact changed Rust source files with their crate's tests:

```
python -m conductor.mutation_campaign_generate plan  rust --verbose
python -m conductor.mutation_campaign_generate write rust
python -m conductor.mutation_campaign_generate write python --only conductor/gate_rollout.py
```

`plan` writes nothing. It reports what `write` would emit, which subjects a
committed campaign already covers, and which have no test named after them.

Check a manifest, then run it:

```
python -m conductor.mutation_engine_generated inspect conductor/mutation_campaigns/<id>.json
python -m conductor.mutation_engine_generated run     conductor/mutation_campaigns/<id>.json \
    --allow-mutations --receipt research/reports/mutation/<id>.json
```

`--allow-mutations` is required: the run executes deliberately broken code. The
run happens inside a disposable snapshot worktree, never in the checkout. The
receipt path must be inside the repository.

A campaign generated for the first time carries
`"survivor_baseline_recorded": false`. Its **first run records its own survivor
set** into the manifest and flips that flag; every later run is scored against
it. Nobody writes that field by hand -- a hand-authored baseline is exactly the
self-grading this system exists to remove. Until this was automatic, a fresh
campaign was red on its first run and on every run after, because with no
baseline every survivor counts as new.

## How a run becomes gate evidence

Every new or behavior-changing test requires a current registered automatic
`PASS` receipt before handoff or landing. A zero process exit is insufficient:
`RATCHET_HELD` retains diagnostic survivor history but is not PASS evidence.

1. Register the manifest path in the central registry or a narrow `registry.d`
   fragment. Registration contains pointers, not hand-authored receipt hashes.
2. Publish the current complete PASS receipt with the runner's `--receipt`
   option into `conductor/mutation_campaigns/receipts/`. Keep diagnostic reruns
   in ignored `research/reports/mutation_testing/`; do not commit every run.

The acceptance rule is stricter than holding the engine's survivor baseline:

| | conductor-generated campaign |
|---|---|
| `mutation_engine` | `fest`, `cargo-mutants`, `mull` |
| accepted when | current complete PASS, zero survivors, consistent outcome accounting |
| what PASS means | every scored mutant was killed; unreached/unviable mutants are reported separately |

Native receipt validation enforces this rule independently. A recorded survivor
baseline remains useful for regression diagnosis, but cannot authorize changed
tests with surviving mutants. Historical patch evidence stays archival until
replacement coverage and all retention consumers have been checked.

A generated receipt additionally has to agree with its manifest on
`test_sha256`, `core_sha256`, `adapter_sha256`, and `scope_guard_sha256`, as well
as runner/source pins, so evidence cannot outlive the code it describes.

Legacy manifest value analysis is not an automatic-engine requirement. Missing
optional generated `test_value` attribution is not legacy value-analysis debt;
supplied attribution must still be valid. This applicability distinction does
not waive current PASS, completeness, or hash checks.

## Traps that are pinned, and why

Each of these produced a confident, wrong number before it was pinned. They are
enforced in the adapters and asserted in `test_mutation_engine_*.py`.

- **Coverage is measured over directories, never a single file.** `--cov=<file>`
  records nothing, so every mutant reads as uncovered. The first fest run was a
  green campaign that had executed zero mutants.
- **Mull needs profile data.** Without `--coverage-info` it cannot tell reached
  from unreached and reports almost everything as survived: 4,616 survivors over
  4,926 mutants. `--include-not-covered` must never be passed.
- **Mull's `--timeout` is milliseconds** while every other engine here takes
  seconds, and its effective budget is `max(baseline * 10, minimum-timeout)`.
  These suites run in under 10 ms, so `baseline * 10` truncated to 0 and produced
  309 spurious timeouts. `--minimum-timeout` is manifest data.
- **Exit codes are not verdicts.** cargo-mutants exits 2 and mull-runner exits
  non-zero whenever any mutant survives, which is the normal case.
- **All three engines default to host-sized parallelism**, and all three jitter
  the survivor set until the width is pinned. fest was measured giving
  4/5/5/6/5 survivors over five identical runs. cargo-mutants workers sharing one
  `CARGO_TARGET_DIR` gave 45/43/46 survivors and 24/21/22 unviable over three
  runs of the same 196 mutants -- whether a mutant compiles cannot legitimately
  vary.
- **The allocator can decide a verdict.** Two identical single-threaded C++ runs
  disagreed on three mutants, because both of them corrupt memory that is then
  read before being written; whether the suite noticed depended on whether glibc
  returned a dirty page. `MALLOC_PERTURB_` makes an uninitialised read
  deterministic, so the mutant stands or falls on what the tests assert.
- **Mutants are named by what they do**, not by where they are:
  `sha256(path \0 operator \0 original \0 mutated)[:12]` plus the occurrence
  index. Line numbers move when anything above them changes, which would churn
  the baseline on every unrelated edit.
- **`PurePath.match` does not treat `**` as recursive before Python 3.13.** A
  trailing `**` behaves like a single `*`, which would silently drop a whole new
  subdirectory from a corpus while the campaign kept reporting green.

## What is measured today

| | in the repo | under a generated campaign |
|---|---|---|
| Python | 2,117 modules, 761,780 lines | 5 modules |
| Rust | 9 crates, ~72,000 lines | 1 crate (196 mutants) |
| C / C++ | `aria_core/src/cpu` | 35 files, 546 mutants |

Measured scores, for calibration: the C++ kernels catch **63%** of generated
bugs, and the mechanical Python sweep catches **61%**. Two unrelated tools on two
languages landing in the same place is the argument that the instrument is
honest. Neither resembles the 1.0 the hand-authored campaigns publish.

## Related

- `KB-MUT-02` -- `research/notes/kb_mutation_campaigns.md`, the rules for
  authoring, running and consuming campaigns at the gate.
- `research/notes/generated_mutation_engine_2026-09-06.md` -- how each of the
  above was measured, including the runs that were wrong first.
