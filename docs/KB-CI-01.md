---
id: KB-CI-01
title: CI Coverage — a Tree Nothing Runs Is a Tree Nothing Checks
tags: [knowledge-card, ci, governance, dependencies]
---

# KB-CI-01: CI Coverage

**Every tree that can change behaviour must be matched by a workflow path filter *and*
exercised by a job in that workflow. Both halves, or the tree ships unchecked.**

## The failure this prevents

A path filter that omits a tree fails silently: no job runs, nothing goes red, nothing
notices. Nothing in a pull request says "this was not checked" — it simply looks like a
change with no relevant checks attached. A governance/tooling tree (the exact machinery
every other change is judged by) can go a full day with **zero** checks running against it
this way, if the workflow that used to cover it was ever disabled and nothing filled the
gap.

Diagnosing this is one experiment: open a change that touches only the tree in question
and see whether any check runs at all. If none do, the tree is unprotected regardless of
what its own tests claim.

## Adding the path filter alone is not the fix

Adding a tree to a workflow's path filter without also giving it a job that actually
exercises it produces a check that triggers and goes green **having tested nothing**. A
green check that cannot fail is worse than no check, because it reads as evidence when it
is not. Add the path filter *and* a job that runs the tree's tests, in the same change.

## What zero CI hides

A tree with no CI coverage routinely hides dependency problems a developer's local
environment never surfaces, because a developer's virtual environment tends to carry
things transitively that a clean install does not:

* A path-restriction/sandboxing check that inadvertently denies test-collection itself,
  because it treats a legitimate system path (like the OS temp directory) as forbidden.
* A module that is unimportable from a truly clean install because a package it uses is
  present locally but never declared as a dependency.
* A CLI tool the review shells out to (a coverage tool, a spell-checker, a static analyzer)
  that is present on a developer's `PATH` but never declared anywhere the CI environment
  would pick it up.

A clean-install import and a clean-environment CI run are the only things that reliably
find these; a developer's own machine, having accumulated tools and packages over time,
never will.

## Rules

* **Adding or moving a top-level package**: update the path filter **and** a job, in the
  same change. Verify by opening a change that touches only that tree.
* **Disabling a workflow orphans every path only it covered.** Record what it covered
  before disabling it, or the gap is silent and someone else discovers it the hard way.
* A job may ship a non-blocking/"continue on error" mode **only** while a specifically
  named defect blocks it outright, with the condition for removing that mode written down
  in the workflow file itself. It is a countdown, not a permanent setting.
* **Test-only tools** (a test runner, a dead-code scanner used only in CI) may live only in
  the CI job's own setup step. **Anything a shipped module actually imports, or that the
  review shells out to at runtime**, belongs in the project's own dependency manifest —
  putting it only in the workflow file hides a real packaging defect from every other
  consumer who installs the package normally.
* A workflow file that is itself listed in its own path filter makes any change to that
  workflow self-verifying. Prefer that shape when practical.
* **The same invariant applies to ignore files.** A tree nothing runs is unchecked; a file
  nothing tracks is unprotected. A broad exclusion by extension or directory name can
  accidentally swallow a required instruction or policy file. A directory-level exclusion
  also makes a later single-file negation inert, because a version-control tool will not
  even look inside an excluded directory to find the file the negation names — exclude by
  `dir/*` plus an explicit `!dir/<file>` re-inclusion instead. Verify by asking the tool
  what it is actually tracking (an explicit "is this path ignored" check, or a before/after
  count of untracked files), never by reading the pattern and assuming it does what it
  looks like it does.

## Closed negatives — stop re-deriving these

* **Do not guess which analyzer is missing** when a review fails on an unavailable
  analyzer. The failure should name it directly — read the actual finding rather than
  guessing and burning a CI round-trip on the wrong fix.
* A throwaway scratch checkout outside the normal working directory cannot reproduce a
  path-guard or `PATH`-availability defect reliably — reproduce in a real checkout that
  mirrors CI's own layout, then iterate locally in seconds instead of full CI round-trips.
* A local gate run from a mostly-empty context graph understates real coverage, because
  graph-driven test selection degrades when the graph has little to work with. CI running
  against a fully-seeded graph is the authority on what actually ran.

## See also

* `KB-GOV-02` — the landing/gate contract this card's CI coverage exists to back up: a
  gate that never runs is exactly as useful as one that always passes.
