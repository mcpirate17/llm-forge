# Native candidate-review latency workloads

`forge candidate benchmark prepare --repo SOURCE --fixture NEW_DIRECTORY`
captures HEAD into a standalone benchmark-owned Git data snapshot. It does not
create a developer worktree, clone, alternates file, or run any check or guest.
Use `--kind index` explicitly to capture staged prospective implementation
instead. Unstaged files are never silently overlaid. Preparation preserves the
captured source ancestry within its declared object budgets and publishes the
fixture only when every selected scenario is complete. The source checkout and
its index are unchanged.

Preparation prints the fixture path and `manifest_sha256`. Author operator grants
outside that fixture with its exact canonical path as `source_root`, explicit
read grants for the fixture and its Git directory, and the normal native review
program, environment, scratch, cgroup and batch budgets. Grants from the original
repository are refused; the benchmark never rewrites them. Any import-provider
pin, guest tools, runner bundle, registry or equivalence input needed by captured
policy must be genuinely provisioned. No host policy is weakened to get a timing.

```
forge candidate benchmark run --fixture DIRECTORY --output REPORT.json
forge candidate benchmark run --fixture DIRECTORY --output REPORT.json \
  --fixture-sha256 PREPARED_MANIFEST_SHA256 --execute \
  --grants /absolute/operator/grants.json --owner SESSION
```

Without `--execute`, run publishes only a plan. The report parent must exist and
be outside the fixture. Execution retains full newly produced sealed native
review receipts in a new private sibling directory and journals completed rows
atomically. `--policy`, `--today` and the review's registry/runner/equivalence
pins are forwarded as explicit options. The full profile can run expensive
guest tools only after the operator requests execution and grants them.

The default scenario names remain `docs-only`, `small-python`, `native-code`,
`dependency`, `large-delete-rename`, `full-review`, in that order. Repeat
`prepare --scenario NAME` to select a subset or add `native-rust`. The existing
C implementation addition remains `native-code`; `native-rust` is a separate
frozen Rust crate implementation edit, not a substitution. Small-python and
full-review share the exact candidate tree, with fast versus full profiles.
The 500-file baseline and 250 deletes/250 renames, doc probe, C input and lockfile
newline edit retain their semantic roles.

Workload revision `native-review-fixtures-v1` intentionally replaces the old
Python coordinator docstring input with frozen Python analysis data. It is
embedded from `native/conductor-native/src/candidate_benchmark/python_input.py`
and placed under captured `package_root/candidate_review/benchmark_probe.py`.
The edit changes one docstring line; the file never coordinates or approves
review. The optional Rust crate lives at `native/benchmark_probe` in the fixture.
The manifest binds every scenario tree, index digest and before/after blob hashes.
These prospective inputs and the native execution engine define a new workload;
legacy latency numbers are not matched parity or performance evidence.

Every cold/warm label means two ordered fresh processes of the loaded native
CLI running the real complete-profile coordinator. Native review caching stays
disabled. Wall time covers the child's lifecycle; Linux wait4 supplies its maximum
individual waited-process RSS peak. Descendant peaks contribute only when
propagated through completed waits; unpropagated descendants are excluded. This
is not simultaneous aggregate tree RSS. There is no Python coordinator or GNU time
process. Recorded review failures and incomplete reviews keep their original
status, findings, checks and evidence in the receipt. Benchmark `status=complete`
means its requested measurements completed, never that reviews passed or approval
was granted. Infrastructure failure makes the report incomplete and exits with
an error; previously completed rows/receipts remain available.

Preparation admits at most 100,000 reachable Git objects, 64 MiB per object and
2 GiB total object content, in addition to existing snapshot limits. Gitlinks,
unknown/duplicate scenarios, fixture collisions, malformed manifests, index,
manifest or grant drift fail loudly. These are semantic Git-content limits,
not a preallocation guarantee for libgit2 or an RSS ceiling. Each receipt is at
most 16 MiB; the aggregate summary is at most 16 MiB. Completion stdout is proven
complete by byte count and limited to 4096 bytes; stderr retention uses the
existing bounded tail capture. A measurement timeout is 1–3600 seconds, followed
by up to 120 seconds for native cancellation cleanup. Forced cleanup failure is
incomplete and explicitly retains fixture/scratch/cgroup recovery context.

Prepared data and receipts are intentional persistent artifacts for grant binding
and inspection. Failed unpublished preparation uses existing Snapshot cleanup;
normal review Stage owners retain their existing resource cleanup. The benchmark
releases only its own fresh per-measurement ownership claims. It never deletes
an operator directory or silently reuses an older receipt.
