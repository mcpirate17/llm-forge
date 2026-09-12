from __future__ import annotations

from dataclasses import FrozenInstanceError
from pathlib import Path

import pytest

from conductor.session_policy import (
    EMPTY_SESSION_POLICY,
    SessionPolicyError,
    load_session_policy,
)

ROOT = Path(__file__).resolve().parents[1]

# Text this test owns outright: a host project's [tool.conductor.session] table can
# say anything, so the round-trip test below must not pin conductor-tooling's own
# suite to one particular host's wording (that was this project's LLM origin, whose
# CLAUDE.md text has no bearing on this package's behaviour).
_HOST_PREAMBLE = (
    "MISSION: exercise the round trip end to end.",
    "SECOND: a second opted-in preamble line.",
)
_HOST_MANDATES = (
    "FIRST_RULE: a standing mandate a host opted into.",
    "SECOND_RULE: a second standing mandate.",
)


def _write_config(root: Path, body: str) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "pyproject.toml").write_text(body, encoding="utf-8")


def _write_session_config(root: Path) -> None:
    import json

    _write_config(
        root,
        "[tool.conductor.session]\n"
        f"preamble = {json.dumps(list(_HOST_PREAMBLE))}\n"
        f"standing_mandates = {json.dumps(list(_HOST_MANDATES))}\n",
    )


def test_a_host_projects_session_policy_round_trips_exactly(tmp_path: Path) -> None:
    """A host that opts in via [tool.conductor.session] gets back exactly its own
    text, immutably -- the mechanism under test, not conductor-tooling's own
    project-specific wording."""
    _write_session_config(tmp_path)
    policy = load_session_policy(tmp_path)
    assert policy.preamble == _HOST_PREAMBLE
    assert policy.standing_mandates == _HOST_MANDATES
    with pytest.raises(FrozenInstanceError):
        policy.preamble = ()  # type: ignore[misc]


def test_this_packages_own_root_has_no_opinion_by_default() -> None:
    """conductor-tooling ships with a neutral default: unless a host's own
    pyproject.toml opts in, load_session_policy renders nothing -- including for
    this package's own repository root, which carries no [tool.conductor.session]
    table of its own."""
    assert load_session_policy(ROOT) is EMPTY_SESSION_POLICY


def test_missing_file_or_session_table_is_generic_empty(tmp_path: Path) -> None:
    assert load_session_policy(tmp_path) is EMPTY_SESSION_POLICY
    _write_config(tmp_path, "[tool.conductor]\n")
    assert load_session_policy(tmp_path) is EMPTY_SESSION_POLICY


def test_missing_repository_is_not_a_generic_policy(tmp_path: Path) -> None:
    with pytest.raises(SessionPolicyError, match="existing directory"):
        load_session_policy(tmp_path / "missing")


@pytest.mark.parametrize(
    "body",
    (
        "[tool",
        "[tool]\nconductor = []\n",
        "[tool.conductor]\nsession = []\n",
        "[tool.conductor.session]\npreamble = []\n",
        "[tool.conductor.session]\npreamble = []\nstanding_mandates = []\nextra = []\n",
        "[tool.conductor.session]\npreamble = [1]\nstanding_mandates = []\n",
        '[tool.conductor.session]\npreamble = []\nstanding_mandates = [""]\n',
        '[tool.conductor.session]\npreamble = [" "]\nstanding_mandates = []\n',
    ),
)
def test_present_policy_is_complete_and_strict(tmp_path: Path, body: str) -> None:
    _write_config(tmp_path, body)
    with pytest.raises(SessionPolicyError):
        load_session_policy(tmp_path)


def test_policy_reader_refuses_an_oversized_or_nonregular_config(
    tmp_path: Path,
) -> None:
    _write_config(tmp_path, "x" * (64 * 1024 + 1))
    with pytest.raises(SessionPolicyError, match="exceeds 64 KiB"):
        load_session_policy(tmp_path)
    (tmp_path / "pyproject.toml").unlink()
    (tmp_path / "pyproject.toml").mkdir()
    with pytest.raises(SessionPolicyError, match="regular file"):
        load_session_policy(tmp_path)
