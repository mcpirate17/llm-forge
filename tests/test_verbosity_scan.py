from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review.verbosity_scan import main

HEAVY = (
    "def heavy(x):\n"
    '    """Docstring."""\n'
    + "".join(f"    # step {i} of the plan\n" for i in range(9))
    + "    return x\n"
)
LIGHT = "def light(x):\n    # one note\n    return x\n"


def test_version(capsys: pytest.CaptureFixture[str]) -> None:
    assert main(["--version"]) == 0
    assert capsys.readouterr().out.startswith("verbosity-scan")


def test_comment_heavy_function_is_reported_and_exits_zero(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    src = tmp_path / "heavy.py"
    src.write_text(HEAVY)
    assert main([str(src)]) == 0
    out = capsys.readouterr().out
    assert f"{src}:1: comment/comment-heavy-function" in out
    assert "1 finding(s)" in out


def test_strict_exits_nonzero_on_a_finding(tmp_path: Path) -> None:
    src = tmp_path / "heavy.py"
    src.write_text(HEAVY)
    assert main(["--strict", str(src)]) == 1


def test_light_function_is_clean_even_when_strict(tmp_path: Path) -> None:
    src = tmp_path / "light.py"
    src.write_text(LIGHT)
    assert main(["--strict", str(src)]) == 0


def test_non_python_paths_are_ignored(tmp_path: Path) -> None:
    other = tmp_path / "notes.txt"
    other.write_text(HEAVY)
    assert main(["--strict", str(other)]) == 0
