from __future__ import annotations

import json
import sys
from collections.abc import Callable
from pathlib import Path
from types import ModuleType

import pytest

from conductor import session_preamble as preamble


@pytest.fixture(autouse=True)
def _isolated_exposure(monkeypatch: pytest.MonkeyPatch) -> None:
    """Keep the EXPOSED line out of live shared state.

    `compact_state` gained an exposure summary that calls
    `workspace_hygiene.cheap_exposure_counts`, which reads the repository's SHARED
    `.git/governance/ownership-claims.json`. That is correct in production and wrong
    in a test: no tmp_path fixture can isolate it, and the path guard rightly refuses
    it (governance reset deliverable 5).

    Stubbed rather than disabled via `include_exposure=False`, so the CLI paths still
    exercise the line's presence and its contribution to the MAX_INJECT_CHARS budget.
    """
    monkeypatch.setattr(
        preamble,
        "_exposure_line",
        lambda: "EXPOSED: 0 local-only commit(s), 0 stale dirty file(s).",
    )


def _install_active_state_stub(
    monkeypatch: pytest.MonkeyPatch,
    save_active_state: Callable[[Path], object],
) -> None:
    module = ModuleType("conductor.active_state")
    module.save_active_state = save_active_state  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "conductor.active_state", module)


def _state() -> dict[object, object]:
    return {
        "standing_mandates": [
            "NOVEL_MECHANISMS_ONLY: never softmax twins.",
            "MEMORY_RETRIEVE: query, do not dump.",
        ],
        "active_headings": ["heading-a", "heading-b"],
        "active_claims": [
            {
                "claim_id": "claim-x",
                "owner": "grok",
                "paths": ["conductor/session_preamble.py"] * 40,
                "justification": "should not appear in inject",
            }
        ],
    }


def test_compact_state_omits_claim_paths() -> None:
    text = preamble.compact_state(_state())  # type: ignore[arg-type]
    assert "NOVEL_MECHANISMS_ONLY" in text
    assert "MEMORY_RETRIEVE" in text
    assert "heading-a" in text
    assert "CLAIMS: 1 active" in text
    assert "session_preamble.py" not in text
    assert "should not appear" not in text
    assert "7317" in text
    assert "zero approval authority" in text
    assert "never gate work or runs on local output" in text
    assert ".current_work.md" in text
    assert "MUTATION" in text
    assert "mutation-coverage" in text


def test_compact_state_states_mutation_authority_and_delegation() -> None:
    text = preamble.compact_state(_state())  # type: ignore[arg-type]

    assert "Mutation runs are pre-approved (Tim, 2026-08-31)" in text
    assert "disposable worktrees only" in text
    assert "DELEGATE: searches touching >3 files" in text
    assert "ast_context_tool/query_graph" in text


def test_inject_stays_under_budget() -> None:
    huge_summary = "peer " * 2000
    text = preamble.render_text(
        state=_state(),  # type: ignore[arg-type]
        a2a_name="grok",
        a2a_summary=huge_summary,
    )
    assert len(text) <= preamble.MAX_INJECT_CHARS
    assert "python -m conductor.kb_retrieve" in text


def test_a2a_summary_requires_explicit_show_for_full_message() -> None:
    text = preamble.render_text(
        state=_state(),  # type: ignore[arg-type]
        a2a_name="codex-efficiency",
        a2a_summary="[open] message-1 from=peer\n  bounded summary",
    )

    assert "A2A compact (codex-efficiency); retrieve only when needed" in text
    assert (
        "`python -m conductor.agent_a2a show --as-name codex-efficiency <id>`" in text
    )
    assert (
        "`python -m conductor.agent_a2a read --as-name codex-efficiency <id>`" in text
    )
    assert "bounded summary" in text


def test_hook_payload_shape() -> None:
    payload = preamble.hook_payload(state=_state())  # type: ignore[arg-type]
    out = payload["hookSpecificOutput"]
    assert out["hookEventName"] == "SessionStart"
    assert "MISSION" in out["additionalContext"]
    json.dumps(payload)


def test_text_cli(tmp_path: Path) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text(json.dumps(_state()), encoding="utf-8")
    rc = preamble.main(["text", "--state", str(state_path)])
    assert rc == 0


def test_load_state_rejects_malformed_explicit_cache(tmp_path: Path) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text("not json", encoding="utf-8")

    with pytest.raises(preamble.PreambleError, match="unreadable"):
        preamble.load_state(state_path, refresh=False)


def test_load_state_refreshes_canonical_cache(monkeypatch) -> None:
    refreshed = preamble.ACTIVE_STATE_PATH

    class FakeState:
        def to_dict(self) -> dict[str, object]:
            return {"schema_version": 1, "last_updated": "fresh"}

    _install_active_state_stub(monkeypatch, lambda path: FakeState())
    assert preamble.load_state(refreshed) == {
        "schema_version": 1,
        "last_updated": "fresh",
    }


def test_load_state_rejects_missing_and_non_object(tmp_path: Path) -> None:
    missing = tmp_path / "absent.json"
    with pytest.raises(preamble.PreambleError, match="does not exist"):
        preamble.load_state(missing, refresh=False)
    path = tmp_path / "active_state.json"
    path.write_text("[]\n", encoding="utf-8")
    with pytest.raises(preamble.PreambleError, match="must be an object"):
        preamble.load_state(path, refresh=False)


def test_compact_state_skips_non_string_mandates() -> None:
    text = preamble.compact_state(
        {
            "standing_mandates": ["KEEP: yes", 12, ""],
            "active_headings": [12, "keep-me"],
            "active_claims": [],
        }
    )
    assert "KEEP" in text
    assert "keep-me" in text


def test_hook_cli_and_load_error(tmp_path: Path, capsys) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text(json.dumps(_state()), encoding="utf-8")
    assert preamble.main(["hook", "--state", str(state_path)]) == 0
    out = capsys.readouterr()
    assert "SessionStart" in out.out
    missing = tmp_path / "nope.json"
    assert preamble.main(["text", "--state", str(missing)]) == 2
    err = capsys.readouterr()
    assert "ERROR" in err.err


def test_load_state_refresh_failure(monkeypatch) -> None:
    def boom(path):
        raise OSError("cannot refresh")

    _install_active_state_stub(monkeypatch, boom)
    with pytest.raises(preamble.PreambleError, match="refresh failed"):
        preamble.load_state(preamble.ACTIVE_STATE_PATH)
