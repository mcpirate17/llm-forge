# Candidate review CLI migration

The installed `candidate_review` Python entry point and its review execution graph are retired. Review and attestation run through Forge; the remaining `conductor.candidate_review` package contains Python ownership, identity, Git/model support, and Vulture helpers still used by other shipped modules. This does not remove those APIs or imply their native migration.

| Previous operation | Forge command | Execution boundary |
| --- | --- | --- |
| `candidate_review review` | `forge candidate review` | Select `--kind`, `--surface`, profile and explicit operator grants. Default invocation plans only; `--execute` runs the complete active profile. `--receipt`, `--sarif-out` and `--junit-out` are opt-in output paths. |
| `attest-message` | `forge candidate attest-message` | Requires grants, `--execute`, and a fresh index/pre-commit/fast review; the canonical receipt and message are bound to that execution. |
| `verify-receipt` | `forge candidate verify-receipt` | Checks digest consistency; `--repo` and `--ref` jointly require matching commit tree. A valid digest is not authorization. |
| `claim`, `touch-claim`, `release-claim`, `claims` | `forge claim create`, `touch`, `release`, `list` | Native operations use the shared ownership store. List retains overlapping path filters, compact summaries and optional path display. |

Native admission, lock and setup failures report a nonzero error without publishing a review receipt or replacing requested outputs. A completed execution that fails can export its own sealed report. No receipt or plan grants approval, and the native path does not infer authority from an existing receipt or automatically discover ungranted tools.

The Python console script is intentionally removed rather than redirected: command shapes, operator grants and explicit execution differ. Use the native subcommands above. Historical Python test and migration records describe their original implementation and do not prove the present native execution ran.

`workspace_hygiene` reads and validates claim rows in process using the retained Python ownership loader and projects out the activity field; mutation campaign generation still uses Python identity and ownership, and the guardrail still calls the Vulture helper. The existing package's `git_source` and `model` remain dependencies of ownership.

For changed-function diagnostics in a configured pytest recording project, native targeted-full review captures each shard's fixture/test-window recorder store in owned scratch after child cleanup. It matches source blobs and intersects per-shard missed functions; collection/import-only activity does not count as a test window. A hit indicates at least one executed line, not full function or branch coverage. Historical standalone test-record APIs remain available.

Native mutation admission uses a strict default: all test definitions in a changed file require evidence unless an operator explicitly pins a valid historical baseline. Waivers additionally require an operator-pinned policy and exact candidate base and file/source hashes. Equivalence execution requires explicitly granted guest and output locations, and review does not infer inherited failure by replaying an ungranted baseline. Candidate policy must resolve inside the selected snapshot; missing policy does not fall back to an installed package. Review locking inherits verified held descriptors, not the former token-only metadata convention.

Owned shard recording selects the nearest captured recorder declaration for each selected test, grouping tests by declaration. Each shard records source paths relative to the captured candidate root while pytest keeps its normal configuration discovery. The selected declaration path and blob identity are bound to the stage. If any shard lacks enabled line recording, function-window attribution is explicitly not evaluated; no aggregate coverage is substituted.
