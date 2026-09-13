# Routing policy: which tier an `Agent` dispatch gets

`docs/roadmap.md` Phase 3 step 2. Decides, from `subagent_type`, `description`
and any requested `model`, which model tier a dispatch is allowed to run at.
Step 1 (ledger plumbing, PR #45/#48) measured the costs this table encodes
against; step 3 (enforcement/`SubagentStop`/outcome join) and step 4 (a gate
metric) are separate, later work.

## Policy table

Approved by Tim 2026-09-13, encoded as data in `ledger/routing_policy.toml`
(loaded at build time via `include_str!`, `policy_version = "2026-09-13.1"`).
`tier_order` ranks the tiers cheapest to priciest: `haiku < sonnet < opus <
fable`.

| class | matched by | tier | cap_tokens |
|---|---|---|---|
| explore | `subagent_type` in `{Explore, claude-code-guide, statusline-setup}`, or description starts with `clerical:` | haiku | 150000 |
| general | `subagent_type` in `{general-purpose, claude}`, or absent/unknown | sonnet | 150000 |
| design | `subagent_type == Plan`, or description starts with `design:` or `rust:` | opus | 150000 |
| inherit | `subagent_type == fork`, or requested model `fable`/`inherit` | **deny** unless description contains `justify:` | 150000 |

A requested `model` above the matched class's own tier is denied unless the
description contains `justify:` anywhere; a requested model at or below the
class tier is allowed exactly as requested. An unknown or absent
`subagent_type` with no matching description prefix falls to `general`
(reason names the fallback explicitly). `inherit`-class dispatches are denied
outright (no tier of their own) unless justified — a `fork` or an explicit
`model: fable`/`inherit` mid-dispatch has no cost ceiling on its own, so the
default posture is to require a reason.

### Deny messages

Deny reasons name the class, the policy's tier for that class, and the exact
edit that clears it:

```
class general (tier sonnet, policy 2026-09-13.1): requested model 'opus' is
above tier; add model: "sonnet" or prefix the description with justify: <reason>
```

```
class inherit (tier deny, policy 2026-09-13.1): denied by default; prefix the
description with justify: <reason> to proceed
```

## `forge route`

Pure decision function, `route(policy, &AgentInput) -> Decision`, exposed as
a CLI subcommand for scripting and tests:

```
forge route --subagent-type Explore [--requested haiku] [--description "..."] [--json]
forge route --subagent-type general-purpose --requested opus --json
# {"class":"general","model":null,"cap_tokens":150000,"decision":"deny",
#  "reason":"...","policy_version":"2026-09-13.1"}
```

`--policy PATH` overrides the embedded `ledger/routing_policy.toml` (used by
tests exercising a malformed or alternate table). A malformed embedded policy
fails loud: `forge route` exits 2 rather than silently allowing.

## The dispatch-seam probe (2026-09-13)

The harness docs leave two things undocumented: whether a `PreToolUse`
hook's `updatedInput` actually changes the `Agent` tool's resolved `model`,
and what identity fields a `PreToolUse` hook sees for a tool call made
*inside* a running subagent. Both were probed directly rather than assumed,
in this scratch clone, with a temporary `.claude/settings.local.json`
registering `PreToolUse` → the release `forge` binary and a
`FORGE_ROUTE_PROBE_MODEL=haiku` env forcing an unconditional
`updatedInput: {..., model: "haiku"}` (merged into the full `tool_input`, not
a bare `{model}` patch — see "does `updatedInput` patch or replace?" below).
The non-interactive driver:

```
claude -p --model sonnet --max-turns 3 --allowedTools "Agent,Read" <<'PROMPT'
Use the Agent tool once with subagent_type Explore and no model field to
list the files in native/forge/src, then stop.
PROMPT
```

### Verdict A: `updatedInput.model` **takes**

The subagent's own transcript
(`<session>/subagents/agent-a4922348ac368160a.jsonl`) shows every assistant
row's `message.model` as:

```json
"model":"claude-haiku-4-5-20251001"
```

even though the coordinator session ran on `--model sonnet` and the `Agent`
call itself carried no `model` field — the hook's `updatedInput` is what put
it on haiku. This is why `hook_outcome_for_agent` (`native/forge/src/route.rs`)
returns `updatedInput` on every allow that has a model to set, rather than
falling back to an `additionalContext`-only advisory.

**Does `updatedInput` patch or replace `tool_input`?** The first probe run
returned a bare `updatedInput: {"model": "haiku"}` and the subagent dispatch
failed its own schema validation three times running ("The parameter
`description` type is expected as `string` but provided as `unknown`", same
for `prompt`) before `--max-turns 3` ran out. `updatedInput` **replaces the
whole `tool_input` object**, not a merge/patch. The fix — and what
`updated_input_with_model` in `route.rs` does — is to clone the original
`tool_input` and override only `model`, so `subagent_type`/`description`/
`prompt` all ride along.

### Verdict B: a hook inside a subagent sees the **parent's own** session identity, plus `agent_id`

A `PreToolUse` hook fired for the `Bash` call the Explore subagent made (to
list the directory) logged, verbatim:

```json
{"agent_id":"a4922348ac368160a","session_id":"6f59a2ad-4405-44c8-9b66-5849152ec629","tool_name":"Bash","transcript_path":"/home/tim/.claude/projects/-mnt-data-llm-scratch-forge-routing-policy/6f59a2ad-4405-44c8-9b66-5849152ec629.jsonl"}
```

`session_id` and `transcript_path` are the **coordinator's own** — identical
to the top-level session file, not a distinct
`<session>/subagents/agent-*.jsonl` path — while `agent_id` is populated and
names the subagent. A `PreToolUse` hook can tell *that* a call is happening
inside a subagent and *which* one, but cannot use `transcript_path` to read
that subagent's own transcript file directly; it would have to derive
`<dirname(transcript_path)>/<session_id>/subagents/agent-<agent_id>.jsonl`
itself. This matters for step 3 (enforcement/outcome join): the join key for
"which dispatch is this a child of" is `agent_id`, not a separate
`transcript_path`.

The probe's stderr-dump code path (gated on `FORGE_ROUTE_PROBE_MODEL`) was
removed before this PR; only the recorded JSON above survives, here and in
the PR body.

## Since the verdict is "takes": no `.claude/agents/*.md` files

Item 5 of the brief only requires per-class `.claude/agents/{explore,
general,design}.md` frontmatter (and a `CLAUDE_CODE_SUBAGENT_MODEL` launch
wrapper doc) if the probe found `updatedInput.model` does **not** take. It
does, so none of that is shipped here — `native/forge/src/dispatch.rs`'s
`PreToolUse` hook is the sole routing mechanism.

## Installing the hook in a project

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Agent",
        "hooks": [{"type": "command", "command": "<path-to>/forge hook PreToolUse"}]
      }
    ]
  }
}
```

`FORGE_ROUTE_DISABLE=1` skips routing entirely (bare allow, no
`additionalContext`, no `updatedInput`) without touching the settings file.

**Not yet installed in the LLM monorepo.** `tooling.hooks.dispatch` (Python)
is LLM's live hook path today and does not invoke `forge` at all; wiring
`forge hook PreToolUse` into LLM's own `.claude/settings.json` is a separate,
later PR (`docs/roadmap.md` step 2b).

## Verification

`native/forge`: `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test` (673 tests, 0 failed) all pass. Median of 10 release
`forge hook PreToolUse` runs with an `Agent` payload on stdin: 1.364 ms
(budget: under 5 ms).

## Enforcement (`docs/roadmap.md` Phase 3 step 3, items 1-2)

Step 2 above only decides a dispatch's tier at the moment it starts. Step 3
watches the dispatch while it runs and finalizes its ledger row once it
stops.

### Which branch of live cap enforcement shipped, and why

Verdict B (above) settled this: a `PreToolUse` fired for a tool call made
*inside* a running subagent carries the **parent's own** `session_id`/
`transcript_path`, never a distinct path for the subagent itself, plus a
populated `agent_id`. There is no harness-provided "this subagent's
transcript" field to read directly. `native/forge/src/cap_enforce.rs`
therefore ships the **derived-path branch**: for any `PreToolUse` whose
payload carries a non-empty `agent_id`, it derives
`<dirname(transcript_path)>/<session_id>/subagents/agent-<agent_id>.jsonl`
itself (`subagent_transcript::derive`) and reads *that* file incrementally.
A payload with no `agent_id` (the overwhelming majority of calls -- anything
outside a subagent) is a fast-path no-op: one field lookup, no filesystem
access at all. Measured in-process median over 10 runs each
(`cap_enforce::timing_probe`, `cargo test --release --bin forge
cap_enforce::timing_probe -- --ignored --nocapture`): fast path **0.03 us**,
derived path (50-line transcript, cold state) **42.92 us** -- both trivial
next to the ~1.4 ms whole-hook budget measured above.

State lives at `<ledger_root>/live/<agent_id>.json`: a byte offset already
folded into the running total, plus the running `billed_total`
(input+output+cache_creation, cache reads excluded, same basis as
`agent_rollup`) and a `warned` flag so the 80% warning fires at most once.
The whole check (state load, incremental read, route lookup, state save) is
bounded to 200 ms wall clock; on overrun the partial read is still
persisted but the call is allowed, with one stderr line -- a slow disk must
never manufacture a denial.

Class and cap resolution prefers the matching `task_dispatch` row for that
`agent_id` (written by the dispatching `Agent` `PreToolUse` call) when one
exists on disk; absent that, it falls back to `forge route` on the
payload's own `agent_type` field, same policy table as step 2.

### Exact deny/warn messages

At 80% of the class cap, once per agent, the call is allowed with an
`additionalContext` warning:

```
at 80% of the 150000 token cap for class general (120000 billed): consider wrapping up soon
```

Over the cap, the call is denied outright:

```
over the 150000 token cap for class general (162345 billed): stop, write your final report now; the parent will re-dispatch what is left
```

Both interpolate the resolved `cap_tokens`, `class`, and the state's current
`billed_total` -- never a hardcoded number.

### `SubagentStop`: finalizing the row

`native/forge/src/subagent_stop.rs` wires a new `SubagentStop` match arm in
`dispatch.rs::run_hook`. On every `SubagentStop`, best-effort (every failure
is one stderr line, never a propagated error -- this hook owns no verdict
and must never block the harness):

1. Reads the payload's `agent_id`, `agent_type`, and derives the subagent's
   own transcript path exactly as the live-enforcement branch above does.
2. Runs `forge ledger rollup-agent <transcript> --agent-id <id>
   [--subagent-type <type>]` as a child bounded to 2 s, over that one
   transcript path only -- never the whole session tree `SessionEnd`'s
   rollup walks. `FORGE_LEDGER_DISABLE=1` skips this entirely.
3. `rollup-agent` upserts the matching `task_dispatch` row keyed by a
   synthetic `agent-<agent_id>` id, with final `billed_tokens`, `tier`, and
   `over_cap = billed_tokens > cap_tokens` (cap resolved the same way as
   step 2, via `forge route` on `subagent_type`/`description`). A second
   `SubagentStop` for the same agent rewrites the same row rather than
   appending a duplicate.
4. Deletes `<ledger_root>/live/<agent_id>.json` -- the agent has stopped, so
   there is nothing left for the next `PreToolUse` to enforce a cap against.
5. Delegates to the Python dispatcher exactly as before -- `SubagentStop`
   owns no verdict of its own, same posture as `SessionEnd`.

**Row identity** (fixed, was debt from PR #52; also in `docs/ledger.md`):
`rollup-agent` has no access to the parent transcript, so it never learns
the real `tool_use_id` a later full `forge ledger rollup --repo` sweep
would key that same dispatch's row under. It writes that synthetic id into
the row's `tool_use_id` field only; the day-file upsert itself is keyed by
`agent_id`, the one field both this live path and the full sweep agree on.
A live row followed by a full sweep of the same dispatch therefore
collapses to one row, not two.

### Installing the hook

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Agent",
        "hooks": [{"type": "command", "command": "<path-to>/forge hook PreToolUse"}]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [{"type": "command", "command": "<path-to>/forge hook SubagentStop"}]
      }
    ]
  }
}
```

`FORGE_LEDGER_DISABLE=1` skips the `SubagentStop` rollup entirely (the
escape hatch for a broken ledger root); it does not affect the live
`PreToolUse` cap check itself -- that check has its own hatch,
`FORGE_CAP_DISABLE=1` (`cap_enforce.rs`), which forces a bare `NoOp`
regardless of how far over cap the subagent already is, without touching
the ledger rollup `FORGE_LEDGER_DISABLE` gates. All three escape hatches:

| Variable | Skips |
|---|---|
| `FORGE_ROUTE_DISABLE=1` | routing (`forge route`, `PreToolUse`'s `additionalContext`/`updatedInput`) |
| `FORGE_LEDGER_DISABLE=1` | the `SessionEnd`/`SubagentStop`-triggered ledger rollup |
| `FORGE_CAP_DISABLE=1` | the live `PreToolUse` cap check only (`cap_enforce.rs`) |
