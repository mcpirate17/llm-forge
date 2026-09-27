"""The public Python planner matches the frozen historical Python fixtures.

Rust checks the same expected manifests and refusals without Python. This test
checks the JSON bridge, output shape, and CampaignError translation used by
`conductor.mutation_campaign_generate.plan()`.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path
from typing import Any

import pytest

from conductor.mutation_campaign_generate import plan
from conductor.mutation_scope import CampaignError

FIXTURES_ROOT = (
    Path(__file__).resolve().parents[2]
    / "native"
    / "conductor-native"
    / "tests"
    / "fixtures"
    / "mutation_plan"
)


def _cases() -> list[str]:
    if not FIXTURES_ROOT.is_dir():
        return []
    return sorted(
        p.name for p in FIXTURES_ROOT.iterdir() if (p / "request.json").is_file()
    )


def _plan_kwargs(request: dict[str, Any]) -> dict[str, Any]:
    return {
        "owner": request["owner"],
        "day": request["day"],
        "jobs": request["jobs"],
        "run_timeout_seconds": request["run_timeout_seconds"],
        "only_sources": request["only_sources"],
        "include_covered": request["include_covered"],
        "extra_tests": request["extra_tests"],
    }


def _run(repo_root: Path, request: dict[str, Any]) -> Any:
    try:
        return plan(request["language"], repo_root=repo_root, **_plan_kwargs(request))
    except CampaignError as exc:
        return exc


@pytest.mark.parametrize("case", _cases())
def test_public_plan_matches_the_frozen_python_corpus(
    case: str, tmp_path: Path
) -> None:
    case_dir = FIXTURES_ROOT / case
    request = json.loads((case_dir / "request.json").read_text(encoding="utf-8"))
    assert request["campaigns_root"] == "conductor/mutation_campaigns", (
        f"{case}: fixture assumes the default campaigns_root; update this test "
        "if a fixture ever overrides it"
    )

    repo_root = tmp_path / "repo"
    shutil.copytree(case_dir / "tree", repo_root)
    result = _run(repo_root, request)

    expected_error = case_dir / "expected_error.txt"
    expected_json = case_dir / "expected.json"

    if expected_error.is_file():
        message = expected_error.read_text(encoding="utf-8").strip()
        assert isinstance(result, CampaignError), f"{case}: public plan did not raise"
        assert str(result) == message, case
        return

    assert not isinstance(result, CampaignError), (
        f"{case}: public plan raised: {result}"
    )

    if expected_json.is_file():
        recorded = json.loads(expected_json.read_text(encoding="utf-8"))
        assert result == recorded, (
            f"{case}: public plan differs from the frozen fixture"
        )


def test_at_least_fifteen_fixture_cases_are_frozen() -> None:
    assert len(_cases()) >= 15
