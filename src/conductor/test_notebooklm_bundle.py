"""The vault root is host-derived and overridable, never a hardcoded home path."""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor import notebooklm_bundle as bundle


def test_vault_root_derives_from_home_unless_overridden(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv(bundle.VAULT_ROOT_ENV, raising=False)
    assert bundle._vault_root() == Path.home() / "Documents" / "CodexVault"
    monkeypatch.setenv(bundle.VAULT_ROOT_ENV, str(tmp_path / "vault"))
    assert bundle._vault_root() == tmp_path / "vault"
