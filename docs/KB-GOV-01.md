---
id: KB-GOV-01
title: Governance Claims, Worktree Isolation & Code Review Graph
tags: [knowledge-card, governance, worktree, claims]
---

# KB-GOV-01: Governance & Worktrees

## The rule

Two independent safety nets, and both are load-bearing:

1. **Claims** say who is editing what, so two agents (or an agent and the host maintainer)
   never modify the same paths unnoticed.
2. **Worktrees are scratch.** A worktree is disposable execution isolation, never a place
   where work is allowed to live.

### Claims

* **Before editing**, register a narrow claim naming the exact paths you are about to touch
  and why. A claim covering a glob or a repository root is not a claim — it is a lock on
  everything, and it should be rejected the same way.
* Claims are shared across every worktree linked to the same checkout, so a claim taken in
  one place is visible from all of them. Claims expire (this platform uses 24h as the
  default lease window); an expired claim is absent, not extended.
* **Before handoff or commit**, re-check the claim is still valid, audit that nothing
  outside its scope changed, then release it. A released claim frees the paths for the
  next agent immediately; an abandoned one blocks them until it expires.
* llm-forge ships the primitives a claim registry is built from —
  `conductor.worktree_lease` (per-worktree lease state), `conductor.workspace_hygiene`
  (reports stale or unleased trees), `conductor.checkout_sync` (keeps a shared checkout
  even with the integration branch) — a host project wires these into its own
  `make governance-claim` / `make governance-check` / `make governance-release-claim`
  style targets, the way this repository wires `make gate` around `conductor.gate`.

### Worktrees are scratch

* **Never install a worktree into a shared virtual environment.** A worktree's own
  `.pth` or editable-install artifacts can leak into a venv other work depends on, and the
  package that "shouldn't have changed" silently starts loading from the wrong tree.
* **Run tools from the tree they belong to.** A relative import resolves against whatever
  directory happens to be first on the module search path, not against the checkout you
  intended — invoke as `<package> -m <module>` from the tree root, or set the interpreter's
  path explicitly, rather than relying on the current working directory.
* **Work that must survive review belongs on a branch**, never hidden inside a scratch
  worktree that nobody else can see and that will eventually be reaped.
* Assert provenance before spending real machine time against a worktree: import the
  module you are about to run heavy work through and check `__file__` resolves under the
  worktree (or checkout) you think it does. A stale editable install pointing somewhere
  else is invisible until something expensive runs against the wrong code.

### Code-review / context graph

If the platform is paired with a structural code graph (call it before Edit/Write —
callers, dependents, impact), treat that call as mandatory ahead of any edit, not optional
convenience. Skipping it is how duplicated functions and partial, uncoordinated writes get
introduced — two agents each thought they were the only one touching a function because
neither checked who else had context on it.

## Why this exists

Every incident behind this card is the same shape: someone worked in a place nobody else
could see (an unleased worktree, an unclaimed path, a skipped graph check), and the
collision surfaced only after real machine time — sometimes GPU-hours or long-running jobs
— had already been spent on it. A claim, a lease, and a mandatory context check are cheap
compared to redoing that work.

## See also

* `KB-GOV-02` — how the resulting change actually lands (branches, the gate, evidence).
* `KB-GOV-07` — the concrete gate sequence to run when landing from a fresh, disposable
  worktree.
