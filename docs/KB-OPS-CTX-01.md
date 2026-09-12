---
id: KB-OPS-CTX-01
title: Context & Token Budget — Inbound and Outbound
tags: [knowledge-card, ops, context, tokens, retrieval, delegation]
---

# KB-OPS-CTX-01: Context & Token Budget

Every token spent reading a repository is a token not spent on the actual task. Two
directions, both cost: **inbound** (what enters an agent's context) and **outbound**
(what it emits).

## Inbound — cheapest tool that answers the question

1. **Graph before raw file reads**, if a structural code/context graph is available:
   locate the symbol, pull its AST context or source directly, then query for callers,
   dependents, or impact — before reading whole files. Ask for the minimal detail level
   first, and only widen it when that is demonstrably insufficient.
2. **Never read a large source file whole** — a few hundred lines is a reasonable
   threshold past which an offset/limited read (or pulling just the relevant symbol) beats
   ingesting the entire file. Do not re-read a file immediately after editing it — a
   successful edit already confirms the change landed.
3. **Retrieve before re-deriving.** Query the knowledge-card index and any memory/notes
   index for prior findings before re-investigating something the project has already
   worked out. A card is invisible to retrieval until its index is rebuilt after the card
   changes.
4. **Never ingest a large standing "current work" or scratch file wholesale.** If the
   project keeps one, a hook should deny reading it outright (shell and direct-read alike)
   and substitute a compact, structured injection instead — a handful of headings and a
   claim *count*, not full paths and full history.
5. **Delegate fan-out.** Searches that touch more than a few files, bulk reads, and
   summarization work belong in a subagent — the raw excerpts stay in that subagent's
   context and only the conclusion returns. Keep the primary context for decisions, not for
   evidence gathering.
6. **Bound what gets pasted into context.** Normalize whitespace, drop exact-duplicate
   fragments, and apply a hard size budget to anything assembled from multiple sources —
   deliberately without summarizing or inferring, since a budget tool that also compresses
   meaning is a different, riskier kind of tool.
7. **Keep any local embedding/inference service small and scoped** — a small context
   window, no persistent keep-alive, and confined to non-primary compute so it never
   competes with real work for the same hardware.

## Outbound — emit the conclusion, not the evidence

* **Cap status updates mechanically.** A structured status-append command should reject a
  body over a fixed line/character limit and a title over a fixed length, rather than
  relying on agents to self-police length.
* **Findings go to a durable notes location**, then into whatever index makes them
  retrievable later — not into chat, and not into a large standing scratch file.
* **Keep knowledge cards short** (tens of lines, not hundreds) with no embedded JSON, CSV,
  or checkpoint dumps. Bulk output belongs in a reports directory the project prunes on a
  schedule, not inline in a card.
* **Do not echo back what you just read or wrote.** A diff stat, a path, and a line number
  carry the same information as a pasted file at a fraction of the cost.
* No effort-signaling prose. A number with a unit beats a paragraph claiming rigor.

## Measurement — these produce the numbers, not opinions

* A read-budget hook (a post-tool-use hook on file reads) can tally reads at a rough
  characters-per-token estimate and emit one advisory line per fixed token step. It should
  never block — it exists to make the delegation rule visible and to decide, from real
  data, whether a hard cap is actually warranted.
* A context-telemetry recorder can log provider-neutral context sizes to a local event log
  — sizes only, never tool contents — so budget decisions are made from measured usage
  rather than a guess.

**Before adding a hard cap anywhere in this pipeline, measure with tools like these first.**
A budget argued from feel is how a limit lands on the wrong tool and gets routed around by
the very agents it was meant to constrain.

## See also

* `KB-GOV-01` — claims and worktree isolation reduce redundant context-gathering the same
  way retrieval-before-read does: check what is already known before spending tokens
  rediscovering it.
