"""A baseline merge may only gain entries."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.baseline_merge import (
    BaselineMergeError,
    assert_no_loss,
    container_key,
    main,
    merge,
    signature,
)


def test_secrets_shape_unions_per_path_without_dropping_absent_files() -> None:
    """The exact 2026-08-29 failure: entries for files this tree lacks survive.

    A regenerate-and-commit prunes `top10_canary.py` because the branch does not
    contain it yet. It reappears higher in the stack and is re-flagged there by
    the entries the merge deleted.
    """
    base = {
        "results": {
            "research/tools/top10_canary.py": [{"type": "Hex", "hashed_secret": "a"}],
            "shared.py": [{"type": "Hex", "hashed_secret": "b"}],
        }
    }
    incoming = {
        "results": {
            "shared.py": [{"type": "Hex", "hashed_secret": "c"}],
            "new_tool.py": [{"type": "Hex", "hashed_secret": "d"}],
        }
    }
    merged = merge(base, incoming)
    assert "research/tools/top10_canary.py" in merged["results"]
    assert len(merged["results"]["shared.py"]) == 2
    assert "new_tool.py" in merged["results"]
    assert not signature(base, "results") - signature(merged, "results")


def test_a_merge_that_would_drop_an_entry_raises_and_names_it() -> None:
    """Refusing is the whole point; a warning would be missed exactly as before."""

    base = {"entries": {"kept": 1, "dropped": 2}}
    merged = merge(base, {"entries": {"added": 3}})
    assert set(merged["entries"]) == {"kept", "dropped", "added"}

    # The guard must RAISE, not warn: a warning is what got missed three times.
    with pytest.raises(BaselineMergeError, match="dropped"):
        assert_no_loss(base, {"entries": {"kept": 1}}, "entries")
    # And it must stay silent when nothing was lost.
    assert_no_loss(base, merged, "entries")


def test_list_shaped_baselines_deduplicate_by_content_not_order() -> None:
    """radon's `findings` is a list; reordering is not loss and repeats are not gain."""
    base = {"findings": [{"f": "a", "rank": "C"}, {"f": "b", "rank": "D"}]}
    incoming = {"findings": [{"rank": "D", "f": "b"}, {"f": "c", "rank": "E"}]}
    merged = merge(base, incoming)
    assert len(merged["findings"]) == 3, "reordered duplicate must not be re-added"


def test_count_is_refreshed_and_container_mismatch_is_refused() -> None:
    base = {"count": 1, "entries": {"a": 1}}
    assert merge(base, {"entries": {"b": 2}})["count"] == 2
    with pytest.raises(BaselineMergeError, match="different container keys"):
        merge({"entries": {}}, {"results": {}})
    with pytest.raises(BaselineMergeError, match="no known container key"):
        container_key({"nothing": 1})


def test_scalar_bucket_on_both_sides_keeps_the_base_value() -> None:
    """A union must not silently overwrite; the base is the tree being merged into."""
    merged = merge({"entries": {"k": "base"}}, {"entries": {"k": "incoming"}})
    assert merged["entries"]["k"] == "base"


def test_a_container_that_is_neither_dict_nor_list_is_refused() -> None:
    """Fail loud on a shape we do not understand rather than merging blindly."""
    with pytest.raises(BaselineMergeError, match="not dict/list"):
        signature({"entries": "a string is not a baseline"}, "entries")


def test_mismatched_container_types_are_refused() -> None:
    """dict-vs-list under the same key is not a union, it is two different files."""
    with pytest.raises(BaselineMergeError, match="container types differ"):
        merge({"entries": {"a": 1}}, {"entries": [{"b": 2}]})


def test_cli_writes_on_merge_and_writes_nothing_under_check(tmp_path) -> None:
    """--check must report the delta and leave the file untouched."""
    base = tmp_path / "base.json"
    incoming = tmp_path / "incoming.json"
    base.write_text(json.dumps({"entries": {"kept": 1}}))
    incoming.write_text(json.dumps({"entries": {"added": 2}}))

    before = base.read_text()
    assert main([str(base), str(incoming), "--check"]) == 0
    assert base.read_text() == before, "--check must not write"

    assert main([str(base), str(incoming)]) == 0
    assert set(json.loads(base.read_text())["entries"]) == {"kept", "added"}

    out = tmp_path / "out.json"
    base.write_text(before)
    assert main([str(base), str(incoming), "--out", str(out)]) == 0
    assert base.read_text() == before, "--out must not touch the base"
    assert set(json.loads(out.read_text())["entries"]) == {"kept", "added"}


@pytest.mark.parametrize(
    "baseline",
    [
        pytest.param("conductor/jscpd_duplication_baseline.json", id="jscpd-entries"),
        pytest.param("conductor/pmd_cpd_duplication_baseline.json", id="pmd-entries"),
        pytest.param("conductor/radon_complexity_baseline.json", id="radon-findings"),
        pytest.param("conductor/vulture_baseline.json", id="vulture-entries"),
        pytest.param(".secrets.baseline", id="secrets-results"),
    ],
)
def test_self_merge_is_an_exact_no_op_invariant(baseline: str) -> None:
    """Merging a real baseline with itself must change nothing, on every shape.

    The invariant that matters: a union is idempotent. If deduplication keyed off
    the wrong side, a self-merge would silently double every entry -- which looks
    like a harmless no-op in review and corrupts the baseline in fact.

    Run against the repo's actual baselines rather than fixtures, because the
    shapes are what they are, not what a fixture author remembers them to be.
    """
    path = Path(__file__).resolve().parents[1] / baseline
    if not path.is_file():
        pytest.skip(f"baseline absent from this tree: {baseline}")
    document = json.loads(path.read_text(encoding="utf-8"))
    key = container_key(document)
    merged = merge(document, document)
    assert signature(merged, key) == signature(document, key)
    assert len(merged[key]) == len(document[key])
