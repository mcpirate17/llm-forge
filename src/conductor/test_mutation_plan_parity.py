"""Twin test: `plan()`'s native and Python paths agree, on the same fixtures
Rust freezes for its own parity test (native/conductor-native/tests/fixtures/mutation_plan/).

This test regenerates -- it calls the real, current Python implementation
(`CONDUCTOR_PLAN_IMPL=python`) and the real, current native implementation
(the default) for each fixture and asserts they agree, so it stays honest as
either side changes. It also checks both against the frozen `expected.json` /
`expected_error.txt` Rust's `cargo test` trusts, so a fixture cannot quietly
drift out of sync with what Python actually does today. Unlike the Rust test,
this one is allowed to -- and does -- spawn Python; that is the whole point of
running it in the Python CI job rather than at `cargo test` time.
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


def _run(
    monkeypatch: pytest.MonkeyPatch, impl: str, repo_root: Path, request: dict[str, Any]
) -> Any:
    if impl == "python":
        monkeypatch.setenv("CONDUCTOR_PLAN_IMPL", "python")
    else:
        monkeypatch.delenv("CONDUCTOR_PLAN_IMPL", raising=False)
    try:
        return plan(request["language"], repo_root=repo_root, **_plan_kwargs(request))
    except CampaignError as exc:
        return exc


@pytest.mark.parametrize("case", _cases())
def test_native_and_python_plan_agree_on_the_frozen_corpus(
    case: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    case_dir = FIXTURES_ROOT / case
    request = json.loads((case_dir / "request.json").read_text(encoding="utf-8"))
    assert request["campaigns_root"] == "conductor/mutation_campaigns", (
        f"{case}: fixture assumes the default campaigns_root; update this test "
        "if a fixture ever overrides it"
    )

    python_root = tmp_path / "python"
    native_root = tmp_path / "native"
    shutil.copytree(case_dir / "tree", python_root)
    shutil.copytree(case_dir / "tree", native_root)

    python_result = _run(monkeypatch, "python", python_root, request)
    native_result = _run(monkeypatch, "native", native_root, request)

    expected_error = case_dir / "expected_error.txt"
    expected_json = case_dir / "expected.json"

    if expected_error.is_file():
        message = expected_error.read_text(encoding="utf-8").strip()
        assert isinstance(python_result, CampaignError), (
            f"{case}: python path did not raise"
        )
        assert isinstance(native_result, CampaignError), (
            f"{case}: native path did not raise"
        )
        assert str(python_result) == message, case
        assert str(native_result) == message, case
        return

    assert not isinstance(python_result, CampaignError), (
        f"{case}: python path raised: {python_result}"
    )
    assert not isinstance(native_result, CampaignError), (
        f"{case}: native path raised: {native_result}"
    )
    assert python_result == native_result, f"{case}: native and python plans disagree"

    if expected_json.is_file():
        recorded = json.loads(expected_json.read_text(encoding="utf-8"))
        assert python_result == recorded, (
            f"{case}: current Python output no longer matches the frozen fixture "
            "-- regenerate native/conductor-native/tests/fixtures/mutation_plan "
            "if this drift is intentional"
        )


def test_at_least_fifteen_fixture_cases_are_frozen() -> None:
    assert len(_cases()) >= 15
