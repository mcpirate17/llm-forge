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
