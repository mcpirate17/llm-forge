from __future__ import annotations

import datetime as dt
import json
from pathlib import Path

import pytest

from conductor.active_state import ActiveState
from conductor.inplace_handoff import (
    hook_context,
    hook_output,
    resolve_identity,
    stage_handoff,
    HandoffError,
    activate_handoff,
    load_handoff,
    prepare_handoff,
    save_handoff,
)

NOW = dt.datetime(2026, 9, 1, tzinfo=dt.UTC)


def _state(*, heading: str = "controlled task") -> ActiveState:
    return ActiveState(
        last_updated=NOW.isoformat(),
        active_headings=[heading],
        active_claims=[
            {
                "claim_id": "claim-owned",
                "owner": "codex",
                "paths": ["conductor/inplace_handoff.py"],
                "justification": "handoff test",
                "expires_at": (NOW + dt.timedelta(hours=2)).isoformat(),
            }
        ],
    )


def _prepared() -> object:
    return prepare_handoff(
        task="continue the controlled task",
        paths=("conductor/inplace_handoff.py",),
        runtime={"adapter": "sidecar", "supports_inplace_replace": False},
        context="verified notes",
        state=_state(),
        now=NOW,
    )


def test_prepare_save_and_activate_is_idempotent_without_releasing_claims(
    tmp_path: Path,
) -> None:
    envelope = _prepared()
    path = tmp_path / "handoff.json"
    save_handoff(envelope, path)

    first = activate_handoff(path, state=_state(), now=NOW)
    second = activate_handoff(path, state=_state(), now=NOW + dt.timedelta(minutes=1))

    assert first.envelope.status == "active"
    assert not first.already_active
    assert second.already_active
    assert second.envelope.activated_at == first.envelope.activated_at
    assert "WORKING CONTEXT (non-authoritative" in first.context
    assert (
        load_handoff(path).active_state["active_claims"]
        == _state().to_dict()["active_claims"]
    )


def test_activate_rejects_tampered_envelope(tmp_path: Path) -> None:
    path = tmp_path / "handoff.json"
    save_handoff(_prepared(), path)
    raw = json.loads(path.read_text(encoding="utf-8"))
    raw["task"] = "tampered"
    path.write_text(json.dumps(raw), encoding="utf-8")

    with pytest.raises(HandoffError, match="integrity"):
        activate_handoff(path, state=_state(), now=NOW)


def test_activate_rejects_stale_live_governance_state(tmp_path: Path) -> None:
    path = tmp_path / "handoff.json"
    save_handoff(_prepared(), path)

    with pytest.raises(HandoffError, match="stale"):
        activate_handoff(path, state=_state(heading="changed coordination"), now=NOW)


def test_context_is_bounded_and_redacts_secret_like_values() -> None:
    envelope = prepare_handoff(
        task="bounded projection",
        context="api_key=should-not-leak sk-abcdefghijklmnop\n" + ("x" * 5_000),
        state=_state(),
        now=NOW,
    )

    assert len(envelope.context) <= 3_500
    assert "should-not-leak" not in envelope.context
    assert "sk-abcdefghijklmnop" not in envelope.context
    assert "[REDACTED]" in envelope.context


def test_prepare_rejects_unsafe_paths() -> None:
    with pytest.raises(HandoffError, match="relative path"):
        prepare_handoff(task="unsafe", paths=("../outside",), state=_state(), now=NOW)


def test_resolve_identity_prefers_explicit_then_env_then_checkout_default(
    tmp_path: Path,
) -> None:
    (tmp_path / ".agents" / "a2a").mkdir(parents=True)
    (tmp_path / ".agents" / "a2a" / "default_identity").write_text("fable-5\n")
    env = {"A2A_AGENT_NAME": "helm"}
    assert resolve_identity("sol", environ=env, root=tmp_path) == "sol"
    assert resolve_identity(environ=env, root=tmp_path) == "helm"
    assert resolve_identity(environ={}, root=tmp_path) == "fable-5"
    assert resolve_identity(environ={}, root=tmp_path / "missing") == "claude"
    with pytest.raises(HandoffError):
        resolve_identity("bad name", environ={}, root=tmp_path)


def test_stage_then_hook_injects_once_and_chains_lineage(tmp_path: Path) -> None:
    state = _state()
    first, path = stage_handoff(
        identity="fable-5",
        task="first",
        context="c1",
        root=tmp_path,
        state=state,
        now=NOW,
    )
    assert path == tmp_path / ".agents" / "handoff" / "fable-5" / "pending.json"
    context = hook_context("fable-5", root=tmp_path, state=state, now=NOW)
    assert context.startswith(f"HANDOFF {first.handoff_id}: first")
    assert "c1" in context
    assert hook_context("fable-5", root=tmp_path, state=state, now=NOW) == ""
    assert hook_context("nobody", root=tmp_path, state=state, now=NOW) == ""
    second, _ = stage_handoff(
        identity="fable-5",
        task="second",
        context="c2",
        root=tmp_path,
        state=state,
        now=NOW,
    )
    assert second.parent_handoff_id == first.handoff_id
    assert hook_output("")["hookSpecificOutput"] == {"hookEventName": "SessionStart"}
    assert hook_output("x")["hookSpecificOutput"]["additionalContext"] == "x"


def test_hook_reports_a_stale_envelope_instead_of_dropping_it(tmp_path: Path) -> None:
    state = _state()
    stage_handoff(
        identity="fable-5",
        task="first",
        context="c1",
        root=tmp_path,
        state=state,
        now=NOW,
    )
    stale = hook_context(
        "fable-5", root=tmp_path, state=_state(heading="changed coordination"), now=NOW
    )
    assert stale.startswith("HANDOFF NOT ACTIVATED for fable-5: handoff is stale")
    assert hook_context("fable-5", root=tmp_path, state=state, now=NOW).startswith(
        "HANDOFF "
    )
