---
id: KB-GOV-07
title: Landing From a Fresh Worktree — Gate Sequence and Verified-Green Discipline
tags: [knowledge-card, governance, worktree, gate, landing]
---

# KB-GOV-07: Landing From a Fresh Worktree

Covers the mechanics specific to landing work from a **fresh, disposable worktree**:
getting the gate to a trustworthy PASS there, and reading that PASS correctly before citing
it. For branch lifecycle, the one-gate rule, and what you block on, see `KB-GOV-02` — this
card does not repeat those rules.

## Setup — use the platform's worktree helper, not a bare `git worktree add`

A wrapped worktree command (fetch, `git worktree add -b <you>/<name>` off the integration
branch, a lease file, environment setup, a context-graph seed) replaces four manual steps
with one, and — critically — writes a lease.

**The lease is the part that matters most.** `git worktree list` says a directory exists;
it never says who made it, what for, or when it should be gone, so trees accumulate silently
and nobody can decide whether the work inside one still matters. A lease file records the
owner, the purpose, and an expiry, written *before* any expensive setup step, so a tree that
fails halfway through setup is still attributable. A hygiene report can then surface two
states it otherwise could not see at all: a tree past its expiry, and a tree with no lease
at all (made outside the wrapped path). Nothing deletes a worktree on expiry — a lease is a
claim about intent, and a wrong one must never cost someone their uncommitted work. Expiring
only makes the tree *reportable*.

Extend a lease that is still doing real work rather than letting it lapse silently; check
lease status with a dry-run report before assuming every worktree is accounted for.

Work only inside the worktree. Never checkout, stash or reset the main shared checkout —
other work routinely has uncommitted state there. Invoke tools as `cd <worktree> && ...`
so relative paths resolve against the worktree, not wherever the module happens to have
been imported from.

## The shared checkout is a mirror

Work lands through a PR from a worktree, so the shared checkout should never be anything
but even with the integration branch. It drifts anyway, because a checkout carrying
hundreds of untracked files looks too risky to fast-forward. It usually is not: a
fast-forward is blocked only by *tracked* files that differ locally and also changed
upstream — untracked files never block one. A sync helper should snapshot the whole working
tree (tracked and untracked alike) to a private ref before moving anything, then attempt a
fast-forward-only merge, reporting rather than acting when that is not possible. A
dry-run mode should say what it would do without touching anything.

## Three environment failures before the gate ever evaluates the change

A fresh worktree fails in ways that look like defects in the change under review but are
pure environment:

1. **Install/build steps first.** A fresh worktree has no built artifacts — dependency
   installs, native extension builds, or any local-binary preflight step needs to run
   before the gate, or a tool-preflight check will refuse on a missing binary that simply
   hasn't been built yet in this tree.
2. **Seed the context/code graph before relying on it.** A fresh worktree starts with an
   empty graph store; the first real call would otherwise trigger a full rebuild (tens of
   minutes on a large tree). Seed it from the main checkout's existing graph instead —
   copying the store and rewriting any absolute paths it recorded is far cheaper than a
   rebuild, and then run an incremental update so the graph's recorded HEAD matches the
   worktree's actual HEAD. The graph is a context tool, not gate evidence — never block a
   landing on a full rebuild; read what you need and proceed.
3. **Run the gate in the foreground, with a hard wall-clock bound.** A backgrounded gate
   run can be killed by something external with no useful signal beyond "terminated." If a
   backgrounded run dies unexpectedly, rerun it in the foreground before concluding
   anything about the change itself.

## The head-stamp gap when only config files changed

A graph updater that only parses source files can skip stamping its recorded HEAD when a
diff contains nothing but non-source changes (pure config/data file edits, for example).
The gate can then still raise a stale-graph failure even though the update ran cleanly and
reported nothing wrong. The fix is to stamp the graph's recorded HEAD directly to the
worktree's actual HEAD rather than re-running the updater again expecting it to notice —
the worktree-seeding helper should already do this comparison and stamp when the two
diverge, saying so on stdout.

## A PASS is not evidence until you read the status

A green gate result routinely means the check measured nothing. Before citing any gate
result as evidence — to land, to report to a peer, to report to the project owner — read
two fields out of the receipt, not just the exit code or the one-line verdict:

* **The per-check status** (`passed` vs `skipped`). A whole-tree scanner can report
  `skipped` when the changed set contains no source it scans — the shape of any docs-only
  or manifest-only change — while the top-line verdict still reads `PASS`. That PASS
  answered a different, easier question than "is this change clean."
* **The size of the change set actually examined.** Comparing an index against its own
  HEAD is a zero-change review that trivially passes without reviewing the branch at all.
  Reviewing a branch needs an explicit base-ref/target-ref comparison across the whole diff.

"It didn't fail" and "it ran and passed" are different claims — only these two fields
distinguish them. When quoting a gate PASS in a landing report, quote the change count and
the per-check statuses alongside the verdict line, not just the verdict line.

## The local gate can see files CI cannot

A local mutation/evidence inventory that is built from tracked *and* untracked files can
resolve a manifest or rank a test file that was never committed. CI clones clean and sees
neither. A local gate passing on a worktree with uncommitted files proves nothing about
what CI will do on the same branch — before trusting any gate-relevant result, build the
actual candidate tree (for example with a scratch index and `git write-tree`) and inspect
that object, not the worktree.

This also catches the atomic-suite-landing failure: landing a new test suite must put the
test file, its mutation-campaign manifest, every generated patch, the receipt, and any
registry entry in the **same commit** — split across commits, the landing itself introduces
a blocker a worktree-local gate run will not show you.

## Push, PR, cleanup

Push the branch, open a PR against the integration branch, wait for it to report mergeable
(bounded — do not poll forever), then let the branch merge and get deleted. Clean up the
worktree and any local lease, fetch, and fast-forward the shared checkout. If the
fast-forward fails, do not reset — report the divergence rather than forcing past
something undiagnosed.

## The commit-hook attestation advisory is expected on worktree-landed PRs

A worktree carries no commit hooks, so CI cannot confirm hooks ran on commits made inside
one. Expect a medium-severity "hooks unverifiable" style advisory on every worktree-landed
PR — this is by design (worktrees are scratch, and the mandatory CI review is the promotion
evidence instead). Do not add hook scaffolding to worktrees to chase this away, and do not
treat it as a finding against the change.

## See also

* `KB-GOV-01` — the claim and lease primitives this card assumes are already in place.
* `KB-GOV-02` — the landing contract this card only supplies the mechanics for.
