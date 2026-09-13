"""Contracts for the fest adapter -- the Python half of the generated runner.

Everything engine-agnostic (naming, scoring, refusals) is proven in
`test_mutation_engine_generated`. What is left here is fest's own report shape
and the coverage handling that a monorepo forces on it.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from conductor.bytecode_isolation import scratch_root_for
from conductor.mutation_engine_fest import _config, _coverage_targets, _environment, _rows
from conductor.mutation_engine_generated import (
    load_generated_campaign,
    resolve_mutant_timeout,
)
from conductor.mutation_pycache_evict import PLUGIN_NAME, SCRATCH_ENV, SOURCES_ENV
from conductor.mutation_scope import CampaignError

WORKTREE = Path("/snap/worktree")
FEST_FIXTURE_MANIFEST = (
    Path(__file__).resolve().parent
    / "testdata/fest/generated_fest_campaign_fixture.json"
)


def mutant(
    *,
    mutator: str = "constant_replace",
    original: str = '"gh"',
    mutated: str = '""',
    offset: int = 0,
    line: int = 1,
    path: str = "conductor/gate_rollout.py",
) -> dict[str, object]:
    """One fest mutant record, in the shape fest's JSON report emits."""

    return {
        "file_path": str(WORKTREE / path),
        "line": line,
        "column": 1,
        "byte_offset": offset,
        "byte_length": len(original),
        "original_text": original,
        "mutated_text": mutated,
        "mutator_name": mutator,
    }


def result(status: str, **kwargs: object) -> dict[str, object]:
    """One fest result record wrapping a mutant."""

    return {
        "mutant": mutant(**kwargs),  # type: ignore[arg-type]
        "status": status,
        "tests_run": [],
        "duration": {"secs": 0, "nanos": 1_000_000},
    }


def result_with_duration(duration: dict[str, int]) -> dict[str, object]:
    """One killed result carrying exactly ``duration``."""

    return {
        "mutant": mutant(),
        "status": "Killed",
        "tests_run": [],
        "duration": duration,
    }


def test_rows_are_ordered_by_position_so_repeats_name_stably() -> None:
    """Two `"gh"` -> `""` mutants in one file are two rows, in file order."""

    report = {"results": [result("Survived", offset=200), result("Killed", offset=100)]}
    rows = _rows(report, WORKTREE)
    assert len(rows) == 2
    assert len({row["id"] for row in rows}) == 2
    assert rows[0]["outcome"] == "KILLED"
    assert rows[1]["outcome"] == "SURVIVED"
    assert rows[0]["path"] == "conductor/gate_rollout.py"


def test_every_receipt_field_a_row_carries_is_pinned() -> None:
    """The row keys are the receipt's schema, and nothing else checks them.

    The survivor baseline matches on `id`; `path`, `line` and `operator` are
    what make a survivor readable to whoever has to close it. `byte_offset` and
    `byte_length` are what attribution re-applies the mutant with, and a line
    number cannot separate two mutants on one line. A rename or a dropped field
    would land silently and only surface as an unreadable ratchet months later.
    """

    (row,) = _rows({"results": [result("Survived", line=42, offset=17)]}, WORKTREE)
    assert set(row) == {
        "id",
        "outcome",
        "path",
        "line",
        "byte_offset",
        "byte_length",
        "operator",
        "original_text",
        "mutated_text",
        "tests_run",
        "duration_seconds",
    }
    assert row["line"] == 42
    assert (row["byte_offset"], row["byte_length"]) == (17, len(row["original_text"]))
    assert row["operator"] == "constant_replace"
    assert row["original_text"] == '"gh"'
    assert row["mutated_text"] == '""'
    assert row["path"] == "conductor/gate_rollout.py"


def test_a_duration_is_recorded_in_seconds_not_in_fests_two_fields() -> None:
    """fest reports secs and nanos separately; a receipt records one number."""

    report = {
        "results": [
            {
                "mutant": mutant(),
                "status": "Killed",
                "tests_run": ["conductor/test_gate_rollout.py::test_one"],
                "duration": {"secs": 3, "nanos": 500_000_000},
            }
        ]
    }
    (row,) = _rows(report, WORKTREE)
    assert row["duration_seconds"] == 3.5
    assert row["tests_run"] == ["conductor/test_gate_rollout.py::test_one"]


def test_a_duration_missing_a_field_is_read_as_zero_not_as_one_second() -> None:
    """Sub-second mutants report only nanos, which is most of a fast suite."""

    report = {"results": [result_with_duration({"nanos": 5_000_000})]}
    assert _rows(report, WORKTREE)[0]["duration_seconds"] == 0.005
    assert (
        _rows({"results": [result_with_duration({})]}, WORKTREE)[0]["duration_seconds"]
        == 0.0
    )


def test_a_duration_is_recorded_to_microseconds() -> None:
    """The recorded precision is the receipt's contract, so it is pinned.

    Mutants run in milliseconds; rounding them to fewer places than this would
    collapse a whole run's costs to the same number.
    """

    report = {"results": [result_with_duration({"secs": 1, "nanos": 234_567_890})]}
    assert _rows(report, WORKTREE)[0]["duration_seconds"] == 1.234568


def test_an_unknown_engine_status_is_refused_not_guessed() -> None:
    """A fest release that adds a status must not be silently scored."""

    with pytest.raises(CampaignError, match="unknown status"):
        _rows({"results": [result("Flaky")]}, WORKTREE)

    # A result carrying no status at all is the same refusal, and the message
    # has to show the empty string rather than a stand-in: "unknown status ''"
    # says the field was absent, which is a different bug from a new spelling.
    missing = result("Killed")
    del missing["status"]
    with pytest.raises(CampaignError, match="unknown status ''"):
        _rows({"results": [missing]}, WORKTREE)


def test_coverage_is_measured_over_directories_never_a_single_file() -> None:
    """`--cov=<file>` records nothing, which reports every mutant as uncovered.

    This is the exact hole the first run of this adapter fell into: a green
    campaign that had executed zero mutants.
    """

    assert _coverage_targets(["conductor/gate_rollout.py"]) == ["conductor"]
    assert _coverage_targets(["component_fab/**/*.py"]) == ["component_fab"]
    assert _coverage_targets(["*.py"]) == ["."]


def test_the_campaign_under_test_is_wired_end_to_end() -> None:
    """A self-contained fixture manifest is loadable and pins a real file.

    llm-forge carries no live fest campaign of its own yet (the registry this
    package resolves via ``conductor.project_paths`` is empty), so this
    manifest exists only to exercise the loader and ``_coverage_targets`` /
    ``_rows`` contracts, scoped to a real module-and-test pair this repo
    actually ships: ``src/conductor/gate_rollout.py``.
    """

    loaded = load_generated_campaign(FEST_FIXTURE_MANIFEST)
    assert loaded.mutation_engine == "fest"
    assert loaded.source == ("src/conductor/gate_rollout.py",)
    assert loaded.survivor_baseline, "the recorded survivor baseline must not be empty"


def test_the_config_carries_the_bound_the_run_resolved() -> None:
    """fest.toml pins a per-mutant timeout, so it must be the effective one.

    The config is written after the baseline on purpose: a manifest that pins
    no bound has one derived from that baseline's wall time, and the config is
    where the engine would otherwise silently fall back to its own default.
    Pinned as the exact document -- every line in it is a bound the engine
    reads, and a dropped or renamed one silently changes what ran.
    """

    loaded = load_generated_campaign(FEST_FIXTURE_MANIFEST)
    loaded.mutant_timeout_seconds = None
    resolve_mutant_timeout(loaded, 0.4)  # a fast suite still gets the floor
    assert _config(loaded, "/v/bin/python") == "\n".join(
        [
            "[fest]",
            'source = ["src/conductor/gate_rollout.py"]',
            'exclude = ["**/test_*.py", "**/conftest.py"]',
            "timeout = 60",
            "seed = 0",
            "workers = 1",
            'test_command = ["/v/bin/python", "-m", "pytest", "-q", '
            '"src/conductor/test_gate_rollout.py"]',
            'output = "json"',
            'backend = "subprocess"',
            "",
        ]
    )


class _EnvironmentCampaign:
    """The two fields `_environment` reads: a declared env and mutated sources."""

    environment: dict[str, str] = {}
    source_sha256: dict[str, str] = {}


def test_the_environment_loads_the_eviction_plugin_for_every_child(
    tmp_path: Path,
) -> None:
    """fest launches its own per-mutant pytest commands; the plugin is the hook.

    No launcher of the adapter's sits between a mutant's rewrite and the child
    that imports it, so `PYTEST_ADDOPTS` must load the eviction plugin inside
    every pytest child this environment reaches, and the two CONDUCTOR
    variables must name this run's scratch and this campaign's mutated files.
    """

    (tmp_path / "src").mkdir()
    campaign = _EnvironmentCampaign()
    campaign.source_sha256 = {"src/conductor/mutated.py": "0" * 64}

    env = _environment(campaign, tmp_path)

    assert env["PYTEST_ADDOPTS"] == f"-p {PLUGIN_NAME}"
    assert env[SCRATCH_ENV] == str(scratch_root_for(tmp_path))
    assert env[SOURCES_ENV] == str(tmp_path / "src/conductor/mutated.py")


def test_a_campaigns_declared_addopts_survive_with_the_plugin_appended(
    tmp_path: Path,
) -> None:
    """A campaign that declared PYTEST_ADDOPTS keeps it, plugin and all."""

    (tmp_path / "src").mkdir()
    campaign = _EnvironmentCampaign()
    campaign.environment = {"PYTEST_ADDOPTS": "--timeout 30"}

    env = _environment(campaign, tmp_path)

    assert env["PYTEST_ADDOPTS"] == f"--timeout 30 -p {PLUGIN_NAME}"


def test_the_environment_binds_the_run_to_this_venv_and_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Every binding the environment exists to make, asserted end to end.

    The snapshot's import roots come first and a declared PYTHONPATH rides
    behind them; VIRTUAL_ENV and PATH pin the venv so fest's bare-`python`
    probe and the test command agree on one interpreter. Dropping PATH here
    also pins the inheritance default, so nothing ambient leaks in.
    """

    (tmp_path / "src").mkdir()
    campaign = _EnvironmentCampaign()
    campaign.environment = {"PYTHONPATH": "declared/extra"}
    monkeypatch.delenv("PATH", raising=False)

    env = _environment(campaign, tmp_path)

    assert env["PYTHONPATH"] == os.pathsep.join(
        [str(tmp_path), str(tmp_path / "src"), "declared/extra"]
    )
    assert env["VIRTUAL_ENV"] == str(Path(sys.executable).parents[1])
    assert env["PATH"] == str(Path(sys.executable).parent) + os.pathsep
