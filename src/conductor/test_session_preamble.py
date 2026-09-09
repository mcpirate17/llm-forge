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
        lambda _repo: "EXPOSED: 0 local-only commit(s), 0 stale dirty file(s).",
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
    # Folded in from test_compact_state_states_mutation_authority_and_delegation,
    # which the 2026-09-09 generated campaign classified MERGE with zero unique
    # kills: it asserted on the same `compact_state` string this one already
    # pins, so the assertions are kept and the redundant nodeid is not.
    assert "mutate ONLY the files you changed" in text
    assert "automatic engines only" in text
    assert "hand-authored mutants" in text
    assert "never repo-wide" in text
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


def _write_session_policy(
    root: Path, preamble_lines: list[str], mandates: list[str]
) -> None:
    import json

    root.mkdir(parents=True, exist_ok=True)
    (root / "pyproject.toml").write_text(
        "[tool.conductor.session]\n"
        f"preamble = {json.dumps(preamble_lines)}\n"
        f"standing_mandates = {json.dumps(mandates)}\n",
        encoding="utf-8",
    )


def test_root_policy_is_rendered_before_live_state_summary() -> None:
    from conductor.session_policy import load_session_policy

    policy = load_session_policy(preamble.ROOT)
    state = {"standing_mandates": list(policy.standing_mandates), "active_claims": []}
    text = preamble.compact_state(state, include_exposure=False)
    expected = "\n".join(
        [
            *policy.preamble,
            "MANDATES: "
            + ", ".join(item.split(":", 1)[0] for item in policy.standing_mandates),
            "CLAIMS: 0 active. Inspect with `make governance-claims`.",
        ]
    )
    assert text == expected


def test_canonical_foreign_state_uses_the_foreign_policy(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repository = tmp_path / "foreign"
    state_path = repository / "conductor" / "active_state.json"
    state_path.parent.mkdir(parents=True)
    _write_session_policy(repository, ["FOREIGN: injected"], ["FOREIGN_RULE: required"])
    state_path.write_text(
        json.dumps(
            {"standing_mandates": ["FOREIGN_RULE: required"], "active_claims": []}
        ),
        encoding="utf-8",
    )
    assert preamble.main(["text", "--state", str(state_path)]) == 0
    output = capsys.readouterr().out
    assert "FOREIGN: injected" in output
    assert "MISSION: Beat frontier models" not in output


def test_noncanonical_state_requires_repo_and_canonical_state_cannot_conflict(
    tmp_path: Path,
) -> None:
    snapshot = tmp_path / "snapshot.json"
    snapshot.write_text(json.dumps(_state()), encoding="utf-8")
    with pytest.raises(SystemExit, match="2"):
        preamble.main(["text", "--state", str(snapshot)])
    first = tmp_path / "first"
    second = tmp_path / "second"
    canonical = first / "conductor" / "active_state.json"
    canonical.parent.mkdir(parents=True)
    canonical.write_text(json.dumps(_state()), encoding="utf-8")
    _write_session_policy(first, ["FIRST"], [])
    _write_session_policy(second, ["SECOND"], [])
    with pytest.raises(SystemExit, match="2"):
        preamble.main(["text", "--state", str(canonical), "--repo", str(second)])
    assert preamble.main(["text", "--state", str(snapshot), "--repo", str(first)]) == 0


def test_text_cli(tmp_path: Path) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text(json.dumps(_state()), encoding="utf-8")
    rc = preamble.main(["text", "--state", str(state_path), "--repo", str(tmp_path)])
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
    assert (
        preamble.main(["hook", "--state", str(state_path), "--repo", str(tmp_path)])
        == 0
    )
    out = capsys.readouterr()
    assert "SessionStart" in out.out
    missing = tmp_path / "nope.json"
    assert (
        preamble.main(["text", "--state", str(missing), "--repo", str(tmp_path)]) == 2
    )
    err = capsys.readouterr()
    assert "ERROR" in err.err


def test_load_state_refresh_failure(monkeypatch) -> None:
    def boom(path):
        raise OSError("cannot refresh")

    _install_active_state_stub(monkeypatch, boom)
    with pytest.raises(preamble.PreambleError, match="refresh failed"):
        preamble.load_state(preamble.ACTIVE_STATE_PATH)


def test_render_without_state_reads_the_selected_repository_without_refresh(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    repository = tmp_path / "foreign"
    _write_session_policy(repository, ["FOREIGN: selected"], ["FOREIGN_RULE: required"])
    calls: list[tuple[Path, bool | None]] = []

    def fake_load(path: Path, *, refresh: bool | None = None) -> dict[str, object]:
        calls.append((path, refresh))
        return {"standing_mandates": ["FOREIGN_RULE: required"], "active_claims": []}

    monkeypatch.setattr(preamble, "load_state", fake_load)
    text = preamble.render_text(repo=repository)
    assert calls == [(repository / "conductor" / "active_state.json", None)]
    assert "FOREIGN: selected" in text


def test_cli_reports_invalid_selected_policy_without_a_traceback(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repository = tmp_path / "foreign"
    state_path = repository / "conductor" / "active_state.json"
    state_path.parent.mkdir(parents=True)
    state_path.write_text(json.dumps(_state()), encoding="utf-8")
    (repository / "pyproject.toml").write_text("[tool", encoding="utf-8")
    assert preamble.main(["text", "--state", str(state_path)]) == 2
    assert "ERROR:" in capsys.readouterr().err
