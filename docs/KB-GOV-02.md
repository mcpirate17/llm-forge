---
id: KB-GOV-02
title: How Work Lands — One Branch, One Gate, Attributed Findings
tags: [knowledge-card, governance, branches, gate, mutation-evidence]
---

# KB-GOV-02: How Work Lands

## The failure this prevents

Governance that only runs at the integration line — never during the work — teaches
agents to route around it: force-push, fan out into a dozen parallel branches, let the
real cost detonate at merge time. **A gate you cannot satisfy by doing your own work well
does not get obeyed; it gets bypassed.** The fix is a single, deliberate, cheap-to-run
gate agents run themselves before pushing, not a control bolted on at the end.

## Branches are temporary

* No branch is permanent unless it is a deliberate fork with a different purpose. A branch
  exists to carry one piece of work to the integration branch and is deleted on merge.
* **Squash-merge trap:** a squash merge preserves file content but breaks commit ancestry.
  Commands that compare commit graphs (`git rev-list`, `git cherry`) will keep reporting a
  squash-merged branch's commits as unlanded. Diff file content against the new target
  instead of trusting either command once a branch has been squash-merged.
* One branch per unit of work, updated in place, deleted on merge. **Never open a second
  branch for the same work** — that is the fan-out this card exists to prevent.
* A branch validator and a hygiene report (age since last push, age since PR was opened)
  are the mechanical way to catch a branch turning permanent before it becomes one.

## One gate

* A single gate command is the verdict, run deliberately before pushing. It should:
  preflight the declared analyzer/tool set, export the tree the same way CI will see it
  (no untracked files leaking in), self-check test configuration, and run the review in
  CI's exact shape — so a local PASS and a CI PASS mean the same thing.
* Local commit hooks should stay disabled by default. A blocking local hook is a required
  check by another name, and reintroducing one is how a single bad hook stops every commit
  repo-wide. Earn a blocking hook back deliberately (green on a clean clone, a rollout
  period, an explicit decision) rather than reinstalling it as a side effect of routine
  setup.

## You block on what you caused

* Every check declares whether it always blocks (`"candidate"` attribution) or blocks only
  when your diff touched the paths it found something in (`"diff"` attribution).
* Findings in files you never opened are **inherited debt**: reported, counted, owned by
  the project — never a blocker on your PR. A whole-tree scanner should split its output
  into what your change caused and what it inherited, and exit non-zero only on the former.
* An unrelated finding blocking your PR is a **gate bug**, not a reason to lower a
  threshold. Fix it with the measurement that demonstrates the bug; never quietly loosen a
  gate to get green.

## Evidence

* **Mutate only the files you changed** and the tests that exercise them — never the whole
  tree. The mutation planner scopes itself to the diff against the merge base by default;
  a whole-tree sweep is a maintenance inventory, not something that runs while landing
  ordinary work. → `KB-MUT-02`
* Mutation evidence comes only from automatic engines run in disposable snapshots.
  Hand-authored mutants, patches, manifests, survivor baselines or receipts are forbidden
  — there is no manual path.
* A changed test with no current receipt is **debt, not a blocker**: land the change,
  record the gap in the PR body and wherever the project tracks outstanding debt.
* Keep receipts for real findings. Generated receipts from routine, passing runs can stay
  out of version control; promote one into the tracked evidence directory only when it
  records a FAIL or a new survivor.
* Coverage should resolve the **newest complete PASS** receipt for a given test path, not
  the first campaign a registry happens to list.

## Reading exceptions and attestation correctly

* An exception/waiver list is typically one inline array in the policy file — splice a new
  entry in rather than appending a whole new duplicate block, and reject an exception whose
  expiry is implausibly far out.
* Some findings should not be waivable at all — for example anything whose location is too
  coarse (a single generic path segment) to specifically identify the finding an exception
  claims to excuse. If an exception looks "stale" (excusing nothing) while the same finding
  keeps failing the PR, the exception was never able to match it — fix the source or the
  scanner's baseline, there is no third option.
* Policy reads the **candidate tree**, not the working tree: uncommitted edits to policy
  or waivers are inert until committed.
* A red integration line hides a PR's own findings — if the review aborts on the base
  branch's own problem, every PR looks broken for the same reason, and its real findings
  stay invisible until the integration branch itself is repaired. Check whether a failure
  is actually yours before spending time on it.
* Commit attestation (the `Agent:` trailer, or whatever your project's authorship
  contract requires) is checked per-commit and closes at the first push. While a commit is
  still unpushed, rewriting it to fix a missing trailer is free (`commit-tree` +
  `update-ref`, or an ordinary amend). After a push, fixing history requires a force-push,
  which is a materially higher-risk action — see `KB-GOV-06`. Add a new commit with the
  correct trailer instead of trying to rewrite what already landed.

## See also

* `KB-GOV-01` — claims and worktree isolation, which this card assumes are already in
  place before work reaches the gate.
* `KB-GOV-07` — the concrete step-by-step gate sequence for landing from a fresh,
  disposable worktree, including pitfalls that look like defects in the change but are
  pure environment.
* `KB-MUT-02` — the full mutation-evidence contract this card only summarizes.
