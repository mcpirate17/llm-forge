---
id: KB-MUT-02
title: Automated Mutation Evidence Only
tags: [knowledge-card, mutation, testing, evidence]
---

# KB-MUT-02: Automated Mutation Evidence Only

## The rule

**Mutate only what you changed.** A mutation campaign covers the files a branch modified
and the tests that exercise them — nothing else. A three-file change is three campaigns.
Running mutation testing across an entire repository while landing an ordinary change is a
defect, not thoroughness: it turns a handful of campaigns into hundreds, and it is how a
mutation-evidence directory ends up with more receipts than the project can review. The
tooling enforces this by default: with no extra flags, the campaign planner plans only
against the diff since the merge base plus any dirty files held under the invoking owner's
own claim. A "plan for everything" flag is an inventory/planning option, never permission
to actually run mutants against unchanged files — the runner independently refuses targets
outside the changed-file scope regardless of what was requested. A changed test may admit
an unambiguous paired production module, never a same-named module in an unrelated package.

Mutation evidence is produced **exclusively** by generated engines running inside
disposable snapshot worktrees. Never hand-author a mutant, a mutation patch, a campaign
manifest, a survivor baseline, or a receipt hash — there is no manual/hand-run path; if one
existed for this platform historically, it has been removed on purpose and is not coming
back as a shortcut.

## Supported engines

| Language | Engine |
|---|---|
| Python | `fest` |
| Rust | `cargo-mutants` |
| C/C++ | Mull |

## Required workflow

1. Make sure the production code you changed has a direct, complete test surface —
   mutation testing measures whether your tests actually kill mutants, it does not create
   coverage that isn't there.
2. **Plan** — shows the campaigns your changed files imply. Costs a fraction of a second;
   writes nothing.
3. **Generate** — writes one manifest per changed subject, still scoped to your change.
4. **Run** — executes a generated campaign through its engine, inside a disposable
   snapshot; the production checkout itself is never mutated.
5. Accept only a machine-generated, current **PASS** receipt, and register it through the
   project's mutation-evidence workflow (commit the manifest, receipts, and registry entry
   together).

Nothing in that sequence is hand-written, including the survivor baseline: the **first**
engine run against a fresh campaign records that campaign's own survivor set into its
manifest, and every later run is scored against that recorded baseline rather than against
zero survivors. Without this, a fresh campaign would be red forever, because with no
baseline every survivor looks like a brand-new one. Retaining that diagnostic baseline does
**not** make a `RATCHET_HELD` status equivalent to `PASS` — a changed or behavior-changing
test still requires zero surviving *tested* mutants against current source, current test,
and current runner bindings before it counts as landing evidence.

## What it costs, roughly

Planning is near-instant regardless of scope. Running is not: mutation testing against
even a single moderately-sized module is a per-mutant test invocation, typically minutes
of wall time per file, not seconds — and running multiple engine workers in parallel has
been observed to make the surviving-mutant set jitter run to run, which defeats using it as
a stable ratchet. That cost is exactly why the per-change scope rule exists, and why
nobody runs this repository-wide as part of ordinary work.

## Evidence and retention

Every new or behavior-changing test needs a registered, current, automatic **PASS** before
landing. Missing evidence, retained survivors, timeouts, baseline failures, and hash drift
between the manifest and the current source all block that specific change — a generic
"we'll get to mutation coverage eventually" posture does not satisfy a project that has
adopted this contract. Historical, repository-wide debt (files that predate the contract
and have no coverage yet) is a separate, read-only inventory that never executes mutants by
itself; work through it in bounded, deliberate cohorts, not as a side effect of landing
unrelated work.

Do not commit a receipt for every diagnostic rerun — that is how a mutation-evidence
directory becomes unreviewable. Preserve historical campaigns and failure evidence until
replacement coverage is current and every consumer that depends on the old evidence has
been checked; being merely absent from a registry listing is not proof that a piece of
evidence is safe to delete. Keep full run output in a task log if you need it, and inspect
compact receipt summaries and the specific survivor rows that matter.

## See also

* `KB-GOV-02` — how mutation evidence fits into the overall landing/gate contract.
* `KB-GOV-07` — running the gate (including mutation evidence checks) from a fresh
  worktree without misreading a `skipped` check as a genuine PASS.
