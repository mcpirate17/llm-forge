# Gate rollout policy

**Never require a check that is not green.**

## What this exists to prevent

On 2026-08-28 `candidate-review` was made a required status check on
`w7-trident-program` and `master`. It had never passed on a real PR.

Each CI cycle (~11 min build + a 634 s sweep) then exposed exactly one new failure
class, with the integration branch frozen behind the gate the whole time:

| Cycle | Failure class |
|---|---|
| 1 | `[[mutation_waivers]]` are inert unless the candidate's base commit equals the pinned `integration_base` — 100 waivers void, 11 CRITICAL findings |
| 2 | Tests read host files: `.claude/.codex/.qwen/.grok` config, gitignored `research/reports/` contracts, `/mnt/data` |
| 3 | Content-addressed pins: one edit to `verification.py` drifted 5 campaigns; registry order shadowed later receipts |
| 4 | A 1200 s CPU budget against a 451-file sweep; `exit -9` reported as test failures |
| 5 | Local ≠ CI: untracked files resolved locally, `.git`-less export in CI, `rg` absent on the runner |
| 6 | Baselines behind the tree: jscpd 293 pairs in baseline vs 602 in tree |

PRs #43–#53 cycled through these. #48 had to be admin-merged through a temporary
ruleset bypass. #53 needed eight rounds. On 2026-08-29 Tim removed the required check
and disabled Governance CI.

None of those failure classes were *wrong*. Every one was a real defect. The mistake
was discovering them through a gate that could block the branch instead of through
one that could only comment.

## The two rules

### Promotion: 5 green merged PRs, as an advisory check, first

A check becomes **required** only after it has passed on **5 merged PRs while
non-required**. The count is consecutive and any red resets it — five greens either
side of a red is evidence of flakiness, which is precisely what must not become
blocking.

Running advisory first is what makes the long tail of failure classes cheap: an
advisory red costs a comment, a required red costs the branch.

Enforced by `python -m conductor.gate_rollout promote`, which refuses below the
threshold. **Promotion is the repository owner's decision** — meeting the threshold
makes a check *eligible*, not promoted. The command additionally requires
`--acknowledge-owner-decision`, so no agent can promote a check on its own judgement.

### Demotion: 2 consecutive reds unrelated to their diffs

A required check that blocks **2 consecutive PRs for reasons unrelated to their
diffs** is demoted automatically, with a handoff entry recording it.

"Unrelated to the diff" is mechanised, not judged. A blocking finding is unrelated
when its `path` is outside the PR's changed files; a finding with no path (a
`governance`-scoped one) is unrelated by construction, because the candidate cannot
have caused it by editing a file. A red where *any* blocking finding names a file the
PR actually changed is related, and does not count toward demotion.

That rule matches the two failures that actually justified it:

- **jscpd**: 600 duplicate pairs in aria C++/YAML that the candidate never opened.
- **waiver base**: every finding named a test file the PR did not touch, because the
  base moved and the waivers went inert.

Demotion is safety rather than policy, so `gate_rollout audit --apply` will take it
without asking. Re-promotion then has to earn its 5 green advisory runs again.

## Commands

```
python -m conductor.gate_rollout status [--check <id>]
python -m conductor.gate_rollout record --check <id> --pr <n> --conclusion success|failure \
    [--required] [--unrelated-to-diff] [--evidence <text>]
python -m conductor.gate_rollout promote --check <id> --ruleset <id> --acknowledge-owner-decision
python -m conductor.gate_rollout demote  --check <id> --ruleset <id> --reason <text>
python -m conductor.gate_rollout audit   --ruleset <id> [--apply]
```

Ledger: `conductor/gate_rollout_ledger.json` (tracked, so the promotion history is
auditable and survives a CI outage).

## Local hooks are covered by the same rule

`core.hooksPath` is currently pointed at an empty directory, so no pre-commit,
commit-msg or post-commit hook runs for any agent or worktree.

**That is deliberate and it stays that way.** The pre-commit `candidate-review` hook
was blocking *every* commit, and disabling it is what made it possible to get back to
two branches after three days. Do not "helpfully" restore it. A future agent that
finds this and switches hooks back on re-creates the pile-up it was turned off to
end.

A blocking local hook is a required check by another name, so it earns its way back
the same way: the gate has to be demonstrably green first, and the hook comes back
advisory before it comes back blocking. The order is

1. `make gate` passes on a clean clone of the integration branch;
2. the pre-commit hook is reinstalled in a non-blocking form (report, exit 0);
3. it accumulates its 5 green merged PRs like any other check;
4. only then may it block a commit, and only on the owner's decision.

Until step 1 holds, `make gate` run deliberately is the evidence, not a hook that
fires on every commit. `workspace_hygiene` reports the hook state so that "commits
are passing" is never mistaken for "commits are being checked" -- the point is that
the disabling is *visible*, not that it is reversed.

## Current state, 2026-08-29

No check is required on either ruleset, and `Governance CI` is disabled. Both are
Tim's decisions and this policy does not reverse them. The rollout ledger starts
empty: whenever a check is re-introduced, it starts advisory and needs its 5 green
merged PRs like anything else — `candidate-review`'s history before the reset does
not count, because it was never green as an advisory check.

Rulesets: `20916148` (w7-trident-program), `21558301` (master).

## What this policy does not do

It does not decide whether a check is *worth* having, and it does not weaken one. A
gate that is wrong gets fixed with the measurement that shows it is wrong — never by
lowering its threshold to make a candidate pass. Demotion is about *where* a check
runs (advisory vs blocking), never about what it checks.
