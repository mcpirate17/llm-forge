"""Paired tests for conductor.mutation_receipt_build.

``_default_receipt_path`` decides where a receipt lands when a campaign runs
without an explicit ``--receipt``. It used to hardcode the monorepo's own
scratch-output literal (``research/reports/mutation_testing``), which a host
without a ``research/`` tree at all -- this repository included -- has nowhere
to write. These tests pin the fix: the directory is resolved through
``[tool.conductor].mutation_receipt_root`` (default preserved byte-for-byte),
created on demand, and any creation failure is a loud ``CampaignError``, never
a silent fallback.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from conductor import mutation_receipt_build as build
from conductor.mutation_scope import CampaignError


def _campaign(campaign_id: str = "fixture_campaign") -> SimpleNamespace:
    """A duck-typed stand-in: ``_default_receipt_path`` reads only ``campaign_id``."""
    return SimpleNamespace(campaign_id=campaign_id)


def _write_pyproject(root: Path, body: str) -> None:
    (root / "pyproject.toml").write_text(body, encoding="utf-8")


def test_default_receipt_path_falls_back_to_the_monorepo_literal_unconfigured(
    tmp_path,
):
    path = build._default_receipt_path(_campaign(), tmp_path)
    directory = tmp_path / "research" / "reports" / "mutation_testing"
    assert path.parent == directory
    assert directory.is_dir()
    assert path.name.startswith("fixture_campaign_")
    assert path.name.endswith(".json")


def test_default_receipt_path_honours_the_configured_mutation_receipt_root(
    tmp_path,
):
    _write_pyproject(
        tmp_path, '[tool.conductor]\nmutation_receipt_root = "campaigns/receipts"\n'
    )
    path = build._default_receipt_path(_campaign("llm_forge_campaign"), tmp_path)
    directory = tmp_path / "campaigns" / "receipts"
    assert path.parent == directory
    assert directory.is_dir()
    assert path.name.startswith("llm_forge_campaign_")


def test_default_receipt_path_creates_a_directory_that_does_not_exist_yet(tmp_path):
    _write_pyproject(
        tmp_path, '[tool.conductor]\nmutation_receipt_root = "a/b/c/receipts"\n'
    )
    directory = tmp_path / "a" / "b" / "c" / "receipts"
    assert not directory.exists()
    build._default_receipt_path(_campaign(), tmp_path)
    assert directory.is_dir()


def test_default_receipt_path_fails_loud_when_the_directory_cannot_be_created(
    tmp_path,
):
    """A plain file sitting where the directory must go blocks ``mkdir`` with an
    ``OSError`` -- this must surface as a ``CampaignError`` naming the directory,
    never a silent fallback to some other location."""
    _write_pyproject(tmp_path, '[tool.conductor]\nmutation_receipt_root = "blocked"\n')
    (tmp_path / "blocked").write_text("not a directory\n", encoding="utf-8")
    with pytest.raises(CampaignError, match="cannot create mutation receipt directory"):
        build._default_receipt_path(_campaign(), tmp_path)


def test_resolve_receipt_path_uses_the_default_when_none_is_given(tmp_path):
    _write_pyproject(
        tmp_path, '[tool.conductor]\nmutation_receipt_root = "campaigns/receipts"\n'
    )
    output_path, relative = build._resolve_receipt_path(
        _campaign("resolved_campaign"), None, tmp_path
    )
    assert output_path.parent == tmp_path / "campaigns" / "receipts"
    assert relative.startswith("campaigns/receipts/resolved_campaign_")


def test_resolve_receipt_path_still_honours_an_explicit_path(tmp_path):
    explicit = tmp_path / "somewhere" / "receipt.json"
    output_path, relative = build._resolve_receipt_path(_campaign(), explicit, tmp_path)
    assert output_path == explicit.resolve()
    assert relative == "somewhere/receipt.json"
