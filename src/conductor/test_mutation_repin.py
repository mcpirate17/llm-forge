"""`repin --campaign` must refuse an id that selects nothing.

`repin --run` takes a manifest *path* and `--campaign` takes a campaign *id*, so
handing the path to the flag is the easy mistake -- and it used to filter every
campaign out and report `{"status": "CLEAN"}`, which reads as "every pin is
current" while a pin was in fact drifted. These live in their own module because
`conductor/test_mutation_testing.py` is under a `complete` scope whose nodeid
inventory is pinned by the framework's own campaign.
"""

from __future__ import annotations

import json
from dataclasses import replace
from pathlib import Path

import pytest

from conductor import mutation_testing
from conductor.test_mutation_testing import (  # noqa: PLC2701
    _temporary_campaign,
    _write_pass_receipt,
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


def _repinned(campaign: mutation_testing.Campaign) -> mutation_testing.Campaign:
    """The campaign as a re-pin leaves it: manifest rewritten, receipt untouched.

    This is the exact live shape -- ten registered campaigns are in it today. The pins
    now describe the tree, so `source_drift` is empty and `repin` used to answer CLEAN,
    while the receipt still describes the manifest that was replaced.
    """
    campaign.manifest_path.write_text('{"repinned": true}\n', encoding="utf-8")
    return replace(
        campaign,
        manifest_sha256=mutation_testing._sha256(campaign.manifest_path),  # noqa: SLF001
    )


def _age_the_runner(tmp_path: Path) -> None:
    """Move the runner out from under a published receipt, as a framework edit does."""
    receipt = tmp_path / "receipts/pass.json"
    payload = json.loads(receipt.read_text("utf-8"))
    payload["runner_components_sha256"] = {
        path: "0" * 64 for path in payload["runner_components_sha256"]
    }
    receipt.write_text(json.dumps(payload), encoding="utf-8")


def test_a_repinned_manifest_makes_its_receipt_stale_while_the_source_reads_clean(
    tmp_path: Path,
) -> None:
    """The blind spot: clean pins, dead evidence, and nothing said so."""
    campaign = _temporary_campaign(tmp_path)
    _write_pass_receipt(tmp_path, campaign)
    registry = _write_registry(tmp_path)
    repinned = _repinned(campaign)

    assert mutation_testing.source_drift(repinned, tmp_path) == []
    stale = mutation_testing.receipt_drift([repinned], tmp_path, registry)

    assert [row["campaign_id"] for row in stale] == ["temporary_campaign"]
    assert "manifest hash mismatch" in stale[0]["errors"]


def test_a_campaign_whose_receipt_the_gate_accepts_is_not_stale(tmp_path: Path) -> None:
    """Positive control: a check that flags everything reports nothing."""
    campaign = _temporary_campaign(tmp_path)
    _write_pass_receipt(tmp_path, campaign)
    registry = _write_registry(tmp_path)

    assert mutation_testing.receipt_drift([campaign], tmp_path, registry) == []


def test_one_acceptable_receipt_outweighs_every_superseded_one(tmp_path: Path) -> None:
    """Superseded receipts stay on disk; the question is whether ANY is good.

    The superseded copy is named to sort FIRST, so a check that stopped at the first
    receipt it read -- or that required all of them to be clean -- fails here.
    """
    campaign = _temporary_campaign(tmp_path)
    _write_pass_receipt(tmp_path, campaign)
    registry = _write_registry(tmp_path)
    superseded = json.loads((tmp_path / "receipts/pass.json").read_text("utf-8"))
    superseded["manifest_sha256"] = "0" * 64
    (tmp_path / "receipts/aaa_superseded.json").write_text(
        json.dumps(superseded), encoding="utf-8"
    )

    assert mutation_testing.receipt_drift([campaign], tmp_path, registry) == []


def test_repin_run_regenerates_a_stale_receipt_without_rewriting_the_manifest(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A stale receipt needs a re-run, not a re-pin.

    A runner component moved, so the receipt is dead while the manifest still describes
    the tree exactly. Re-pinning it would rewrite bytes for no reason -- and on a
    manifest carrying `value_analysis`, whose key order is semantic, that rewrite is how
    a hand-authored contract quietly loses its meaning.
    """
    campaign = _temporary_campaign(tmp_path)
    _write_pass_receipt(tmp_path, campaign)
    _age_the_runner(tmp_path)
    registry = _write_registry(tmp_path)
    before = campaign.manifest_path.read_bytes()
    monkeypatch.setattr(mutation_testing, "load_campaign", lambda *_a, **_k: campaign)

    def refuse(*_args: object, **_kwargs: object) -> dict[str, str]:
        raise AssertionError("a stale receipt must not re-pin the manifest")

    monkeypatch.setattr(mutation_testing, "_plan_repin", refuse)

    def run_campaign(_campaign: object, **kwargs: object) -> dict[str, object]:
        path = kwargs["receipt_path"]
        assert isinstance(path, Path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("{}\n", encoding="utf-8")
        return {"status": "PASS", "receipt_path": str(path)}

    monkeypatch.setattr(mutation_testing, "run_campaign", run_campaign)

    result = mutation_testing.repin_campaigns(
        registry, run=True, allow_mutations=True, repo_root=tmp_path
    )

    assert result["status"] == "REPINNED"
    assert result["drifted"] == []
    assert [row["campaign_id"] for row in result["rerun"]] == ["temporary_campaign"]
    assert campaign.manifest_path.read_bytes() == before


def test_a_targeted_repin_survives_another_lanes_unloadable_manifest(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Naming one campaign must not make the repair hostage to an unrelated manifest."""
    campaign = _temporary_campaign(tmp_path)
    campaign.manifest_path.write_text(
        json.dumps({"campaign_id": campaign.campaign_id}), encoding="utf-8"
    )
    (tmp_path / "broken.json").write_text("{ not json", encoding="utf-8")
    registry = tmp_path / "registry.json"
    registry.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "enforcement": "changed_tests",
                "test_patterns": list(mutation_testing.CANONICAL_TEST_PATTERNS),
                "receipt_directories": ["receipts"],
                "campaigns": [
                    {"manifest": "campaign.json"},
                    {"manifest": "broken.json"},
                ],
            }
        ),
        encoding="utf-8",
    )

    def load_campaign(manifest: Path, **_kwargs: object) -> mutation_testing.Campaign:
        if manifest.name == "broken.json":
            raise mutation_testing.CampaignError("broken.json is not a manifest")
        return campaign

    monkeypatch.setattr(mutation_testing, "load_campaign", load_campaign)

    result = mutation_testing.repin_campaigns(
        registry, campaign_ids=[campaign.campaign_id], repo_root=tmp_path
    )

    assert result["status"] == "CLEAN"
