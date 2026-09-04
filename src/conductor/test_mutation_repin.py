"""`repin --campaign` must refuse an id that selects nothing.

`repin --run` takes a manifest *path* and `--campaign` takes a campaign *id*, so
handing the path to the flag is the easy mistake -- and it used to filter every
campaign out and report `{"status": "CLEAN"}`, which reads as "every pin is
current" while a pin was in fact drifted. These live in their own module because
`conductor/test_mutation_testing.py` is under a `complete` scope whose nodeid
inventory is pinned by the framework's own campaign.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor import mutation_testing
from conductor.test_mutation_testing import (  # noqa: PLC2701
    _temporary_campaign,
    _write_registry,
)


def test_repin_refuses_a_campaign_id_that_matches_nothing(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)

    with pytest.raises(mutation_testing.CampaignError) as excinfo:
        mutation_testing.repin_campaigns(
            registry,
            campaign_ids=["conductor/mutation_campaigns/campaign.json"],
            repo_root=tmp_path,
        )

    assert "conductor/mutation_campaigns/campaign.json" in str(excinfo.value)


def test_repin_still_selects_a_campaign_id_that_matches(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Positive control: the refusal must not swallow a legitimate selection."""
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)

    result = mutation_testing.repin_campaigns(
        registry,
        campaign_ids=[campaign.campaign_id],
        repo_root=tmp_path,
    )

    assert result["status"] == "CLEAN"


def _drifted_once(monkeypatch: pytest.MonkeyPatch) -> None:
    """Report drift on the first look and a clean tree afterwards.

    That is the shape `repin --run` walks: a campaign drifts, the manifest is re-pinned,
    and the re-pinned campaign no longer drifts -- which is what lets it reach the
    re-run instead of refusing with "still drifted after re-pin".
    """
    seen: list[int] = []

    def source_drift(*_args: object, **_kwargs: object) -> list[dict[str, str]]:
        seen.append(1)
        return [{"path": "source.py"}] if len(seen) == 1 else []

    monkeypatch.setattr(mutation_testing, "source_drift", source_drift)
    monkeypatch.setattr(
        mutation_testing, "_plan_repin", lambda *_a, **_k: {"campaign.json": "{}\n"}
    )


def test_repin_run_publishes_its_receipt_where_the_evidence_gate_reads(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The regenerated receipt must land in the registry's receipt directory.

    It used to fall back to `research/reports/mutation_testing/`, which is gitignored:
    the re-run reported PASS and the evidence corpus gained nothing.
    """
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    _drifted_once(monkeypatch)
    written: list[Path] = []

    def run_campaign(_campaign: object, **kwargs: object) -> dict[str, object]:
        path = kwargs["receipt_path"]
        assert isinstance(path, Path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("{}\n", encoding="utf-8")
        written.append(path)
        return {"status": "PASS", "receipt_path": str(path)}

    monkeypatch.setattr(mutation_testing, "run_campaign", run_campaign)

    result = mutation_testing.repin_campaigns(
        registry, run=True, allow_mutations=True, repo_root=tmp_path
    )

    assert result["status"] == "REPINNED"
    assert [path.parent for path in written] == [tmp_path / "receipts"]
    assert result["rerun"][0]["receipt"] == str(written[0])


def test_repin_run_refuses_a_rerun_that_published_no_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A PASS with nothing on disk is the failure this command existed to prevent."""
    campaign = _temporary_campaign(tmp_path)
    registry = _write_registry(tmp_path)
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)
    _drifted_once(monkeypatch)
    monkeypatch.setattr(
        mutation_testing,
        "run_campaign",
        lambda *_a, **_k: {"status": "PASS", "receipt_path": "nowhere.json"},
    )

    with pytest.raises(mutation_testing.CampaignError) as excinfo:
        mutation_testing.repin_campaigns(
            registry, run=True, allow_mutations=True, repo_root=tmp_path
        )

    assert "published no receipt" in str(excinfo.value)
