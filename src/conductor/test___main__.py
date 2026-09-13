from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor import __main__ as entry


def _healthy() -> dict[str, object]:
    from tooling.hooks.dispatch import registry

    payload: dict[str, object] = dict(registry.settings_block())
    payload["env"] = {"BASH_QUIET_LIMIT_BYTES": "8000"}
    payload["subagentPromptCacheTtl"] = "1h"
    return payload


def test_no_arguments_prints_usage_and_exits_two(
    capsys: pytest.CaptureFixture[str],
) -> None:
    assert entry.main([]) == 2
    assert "usage: python -m conductor" in capsys.readouterr().out


def test_help_exits_zero(capsys: pytest.CaptureFixture[str]) -> None:
    assert entry.main(["-h"]) == 0
    assert "usage: python -m conductor" in capsys.readouterr().out


def test_unknown_subcommand_exits_two(capsys: pytest.CaptureFixture[str]) -> None:
    assert entry.main(["frobnicate"]) == 2
    assert "unknown subcommand 'frobnicate'" in capsys.readouterr().err


def test_every_subcommand_maps_to_a_module_with_a_main() -> None:
    import importlib

    for name, module_name in entry.SUBCOMMANDS.items():
        module = importlib.import_module(module_name)
        assert callable(module.main), f"{name} -> {module_name} has no main()"


def test_doctor_is_reachable_through_the_entry_point(tmp_path: Path) -> None:
    settings = tmp_path / ".claude" / "settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(json.dumps(_healthy()) + "\n", encoding="utf-8")
    exit_code = entry.main(
        [
            "doctor",
            "--harness",
            "--project-dir",
            str(tmp_path),
            "--home",
            str(tmp_path / "home"),
        ]
    )
    assert exit_code == 0
