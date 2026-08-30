"""The runner lineage narrows an over-broad pin without becoming a bypass.

Every receipt pins the whole-file sha256 of five runner components, so ANY edit to the
runner voids every receipt at once. Measured 2026-08-30: one added comment line dropped
coverage from 505 to 74, rejecting 431 test paths.

`conductor/mutation_runner_lineage.json` records prior runner hash-sets declared
receipt-compatible. These tests pin the properties that keep that honest: it accepts
ONLY exactly-recorded sets, it fails closed on anything malformed or missing, and a
near-miss is still a miss. Every boundary has a fixture on both sides -- a lineage that
accepted too much would silently revive stale evidence, which is worse than the
over-broad pin it replaces.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.mutation_testing import RUNNER_LINEAGE_PATH, _lineage_accepts

RECORDED = {
    "audit/orchestrator/snapshot_worktree.py": "a" * 64,
    "conductor/mutation_scope.py": "b" * 64,
    "conductor/mutation_testing.py": "c" * 64,
    "conductor/mutation_testing_support.py": "d" * 64,
    "conductor/mutation_value.py": "e" * 64,
}


def _write_lineage(repo: Path, payload: object) -> None:
    path = repo / RUNNER_LINEAGE_PATH
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


def _valid(entries: list[dict[str, object]] | None = None) -> dict[str, object]:
    return {
        "schema_version": 1,
        "entries": entries
        if entries is not None
        else [{"id": "e1", "runner_components_sha256": dict(RECORDED)}],
    }


# ---------------------------------------------------------------------------
# Acceptance -- both sides
# ---------------------------------------------------------------------------


def test_accepts_an_exactly_recorded_hash_set(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid())
    assert _lineage_accepts(dict(RECORDED), tmp_path) is True


def test_rejects_a_hash_set_that_was_never_recorded(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid())
    other = dict(RECORDED, **{"conductor/mutation_value.py": "f" * 64})
    assert _lineage_accepts(other, tmp_path) is False


def test_one_differing_component_is_still_a_miss(tmp_path: Path) -> None:
    """A near-miss is a miss. Four of five matching must not be 'close enough'."""
    _write_lineage(tmp_path, _valid())
    near = dict(RECORDED, **{"conductor/mutation_scope.py": "0" * 64})
    assert _lineage_accepts(near, tmp_path) is False


def test_a_subset_is_not_accepted(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid())
    subset = {k: v for k, v in list(RECORDED.items())[:3]}
    assert _lineage_accepts(subset, tmp_path) is False


def test_a_superset_is_not_accepted(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid())
    superset = dict(RECORDED, **{"conductor/extra.py": "9" * 64})
    assert _lineage_accepts(superset, tmp_path) is False


def test_matches_any_entry_not_only_the_first(tmp_path: Path) -> None:
    older = {"id": "old", "runner_components_sha256": {"x": "1" * 64}}
    _write_lineage(tmp_path, _valid([older, {"id": "e1", "runner_components_sha256": dict(RECORDED)}]))
    assert _lineage_accepts(dict(RECORDED), tmp_path) is True


# ---------------------------------------------------------------------------
# Fail-closed -- the property that stops this being a bypass
# ---------------------------------------------------------------------------


def test_absent_file_accepts_nothing(tmp_path: Path) -> None:
    """The control experiment in miniature: no lineage, no acceptance."""
    assert _lineage_accepts(dict(RECORDED), tmp_path) is False


def test_malformed_json_accepts_nothing(tmp_path: Path) -> None:
    path = tmp_path / RUNNER_LINEAGE_PATH
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not json", encoding="utf-8")
    assert _lineage_accepts(dict(RECORDED), tmp_path) is False


def test_unknown_schema_version_accepts_nothing(tmp_path: Path) -> None:
    _write_lineage(tmp_path, {"schema_version": 999, "entries": [{"runner_components_sha256": dict(RECORDED)}]})
    assert _lineage_accepts(dict(RECORDED), tmp_path) is False


def test_entries_not_a_list_accepts_nothing(tmp_path: Path) -> None:
    _write_lineage(tmp_path, {"schema_version": 1, "entries": {"runner_components_sha256": dict(RECORDED)}})
    assert _lineage_accepts(dict(RECORDED), tmp_path) is False


def test_empty_entries_accepts_nothing(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid([]))
    assert _lineage_accepts(dict(RECORDED), tmp_path) is False


def test_non_dict_entry_is_skipped_not_fatal(tmp_path: Path) -> None:
    _write_lineage(tmp_path, _valid(["garbage", {"id": "e1", "runner_components_sha256": dict(RECORDED)}]))
    assert _lineage_accepts(dict(RECORDED), tmp_path) is True


@pytest.mark.parametrize("recorded", [None, "string", 42, ["list"]])
def test_a_non_mapping_recorded_value_is_refused(tmp_path: Path, recorded: object) -> None:
    """A receipt whose runner map is not a mapping cannot match anything."""
    _write_lineage(tmp_path, _valid())
    assert _lineage_accepts(recorded, tmp_path) is False


# ---------------------------------------------------------------------------
# The shipped lineage file is well-formed
# ---------------------------------------------------------------------------


def test_shipped_lineage_is_wellformed_and_documented() -> None:
    repo = Path(__file__).resolve().parents[1]
    path = repo / RUNNER_LINEAGE_PATH
    if not path.is_file():
        pytest.skip(f"{RUNNER_LINEAGE_PATH} is not present in this tree")
    payload = json.loads(path.read_text(encoding="utf-8"))
    assert payload["schema_version"] == 1
    assert payload["entries"], "an empty lineage should be deleted, not shipped"
    for entry in payload["entries"]:
        # Every entry must carry its own justification and evidence, or it is an
        # unaudited bypass rather than a narrowed pin.
        assert entry.get("justification"), f"{entry.get('id')} has no justification"
        assert entry.get("verified_by"), f"{entry.get('id')} has no verification evidence"
        assert entry.get("covers_diff"), f"{entry.get('id')} does not name the diff it covers"
        hashes = entry["runner_components_sha256"]
        assert len(hashes) == 5, "a lineage entry must pin all five runner components"
        assert all(len(v) == 64 for v in hashes.values())
