"""Decision algebra for one hook event: fold every hook's JSON into one response.

Rules (each is a mutation-campaign contract):
- ``permissionDecision``: the strongest wins, ``deny`` > ``ask`` > ``allow``; the
  reasons of every hook that voted the winning decision are joined.
- ``additionalContext`` strings concatenate in hook order, blank-line separated.
- A hook error (exception, timeout, non-zero exit, non-JSON stdout) is never a
  silent skip: it lands in ``systemMessage`` (user-visible) and, for a PreToolUse
  hook marked fail-closed, becomes a ``deny``; otherwise it is also injected as
  context so the agent sees it.
- Top-level ``decision: block`` (PostToolUse) survives with its reasons joined;
  ``continue: false`` wins; ``suppressOutput`` ORs; ``updatedToolOutput`` (and
  the Codex ``updatedMCPToolOutput``) take the last writer and flag a conflict.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Final

from conductor._native import hook_merge_native

SEPARATOR: Final[str] = "\n\n"
_RANK: Final[dict[str, int]] = {"allow": 1, "ask": 2, "deny": 3}
_REWRITE_KEYS: Final[tuple[str, ...]] = ("updatedToolOutput", "updatedMCPToolOutput")


@dataclass(frozen=True)
class HookOutcome:
    name: str
    output: dict[str, Any] | None
    error: str | None = None
    fail_closed: bool = False
    elapsed_ms: float = 0.0


def error_line(outcome: HookOutcome) -> str:
    return f"HOOK ERROR [{outcome.name}]: {outcome.error}"


def _specific(output: dict[str, Any]) -> dict[str, Any]:
    specific = output.get("hookSpecificOutput")
    return specific if isinstance(specific, dict) else {}


def merge(event: str, outcomes: list[HookOutcome]) -> dict[str, Any]:
    payload = [
        {"name": outcome.name, "output": outcome.output, "error": outcome.error,
         "fail_closed": outcome.fail_closed}
        for outcome in outcomes
    ]
    return json.loads(hook_merge_native(event, json.dumps(payload)))


def _merge_reference(event: str, outcomes: list[HookOutcome]) -> dict[str, Any]:
    votes: list[tuple[str, str]] = []
    contexts: list[str] = []
    system: list[str] = []
    block_reasons: list[str] = []
    stop_reasons: list[str] = []
    rewrites: dict[str, Any] = {}
    extra_specific: dict[str, Any] = {}
    stop = False
    suppress = False

    for outcome in outcomes:
        if outcome.error:
            line = error_line(outcome)
            system.append(line)
            if event == "PreToolUse" and outcome.fail_closed:
                votes.append(("deny", line))
            else:
                contexts.append(line)
        output = outcome.output
        if not isinstance(output, dict):
            continue
        specific = _specific(output)
        decision = specific.get("permissionDecision")
        if decision in _RANK:
            votes.append(
                (decision, str(specific.get("permissionDecisionReason") or ""))
            )
        context = specific.get("additionalContext")
        if isinstance(context, str) and context:
            contexts.append(context)
        for key in _REWRITE_KEYS:
            if key in specific:
                if key in rewrites:
                    system.append(
                        f"HOOK CONFLICT [{outcome.name}]: {key} already set by an earlier hook; "
                        "the later value wins"
                    )
                rewrites[key] = specific[key]
        for key, value in specific.items():
            if key not in (
                "hookEventName",
                "permissionDecision",
                "permissionDecisionReason",
                "additionalContext",
                *_REWRITE_KEYS,
            ):
                extra_specific[key] = value
        top_decision = output.get("decision")
        if top_decision in ("block", "deny"):
            reason = str(output.get("reason") or "")
            if event == "PreToolUse":
                votes.append(("deny", reason))
            else:
                block_reasons.append(reason)
        if output.get("continue") is False:
            stop = True
            if output.get("stopReason"):
                stop_reasons.append(str(output["stopReason"]))
        if output.get("suppressOutput") is True:
            suppress = True
        message = output.get("systemMessage")
        if isinstance(message, str) and message:
            system.append(message)

    specific_out: dict[str, Any] = {"hookEventName": event}
    if votes:
        winner = max(votes, key=lambda vote: _RANK[vote[0]])[0]
        specific_out["permissionDecision"] = winner
        reasons = [
            reason for decision, reason in votes if decision == winner and reason
        ]
        if reasons:
            specific_out["permissionDecisionReason"] = SEPARATOR.join(reasons)
    if contexts:
        specific_out["additionalContext"] = SEPARATOR.join(contexts)
    specific_out.update(rewrites)
    specific_out.update(extra_specific)

    result: dict[str, Any] = {"hookSpecificOutput": specific_out}
    if block_reasons:
        result["decision"] = "block"
        result["reason"] = SEPARATOR.join(reason for reason in block_reasons if reason)
    if stop:
        result["continue"] = False
        if stop_reasons:
            result["stopReason"] = SEPARATOR.join(stop_reasons)
    if suppress:
        result["suppressOutput"] = True
    if system:
        result["systemMessage"] = SEPARATOR.join(system)
    return result
