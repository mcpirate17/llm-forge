"""Command construction and skip rules for the line-scoped clang checks.

Every assertion here is about scope: which lines clang is pointed at, and which
files it is not pointed at. Getting that wrong turns a converging formatter
into a wall of findings about lines nobody touched.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from conductor.candidate_review import clang_files as mod


def test_format_command_passes_one_lines_flag_per_range() -> None:
    command = mod._format_command("clang-format", "f.c", [(3, 5), (9, 9)])
    assert command == [
        "clang-format",
        "--style=file",
        "--dry-run",
        "--Werror",
        "--lines=3:5",
        "--lines=9:9",
        "f.c",
    ]


def test_format_command_without_ranges_covers_the_whole_file() -> None:
    command = mod._format_command("clang-format", "f.c", None)
    assert not [item for item in command if item.startswith("--lines")]
    assert command[-1] == "f.c"


def test_tidy_is_pointed_at_the_directory_holding_the_database() -> None:
    command = mod._tidy_command(
        "clang-tidy", "a/b/f.cpp", [(2, 4)], Path("/db/compile_commands.json")
    )
    assert "-p=/db" in command


def test_tidy_line_filter_names_the_basename_and_its_ranges() -> None:
    command = mod._tidy_command(
        "clang-tidy", "a/b/f.cpp", [(2, 4)], Path("/db/compile_commands.json")
    )
    filters = [c for c in command if c.startswith("-line-filter=")]
    assert json.loads(filters[0].split("=", 1)[1]) == [
        {"name": "f.cpp", "lines": [[2, 4]]}
    ]


def test_tidy_without_ranges_sets_no_line_filter() -> None:
    command = mod._tidy_command("clang-tidy", "f.cpp", None, Path("/db/x.json"))
    assert not [c for c in command if c.startswith("-line-filter")]


def test_tidy_warnings_are_errors_so_a_finding_fails_the_check() -> None:
    assert "--warnings-as-errors=*" in mod._tidy_command(
        "clang-tidy", "f.cpp", None, Path("/db/x.json")
    )


def test_compile_database_beside_the_file_wins(tmp_path: Path) -> None:
    (tmp_path / "src").mkdir()
    db = tmp_path / "src" / mod.COMPILE_DB
    db.write_text("[]", encoding="utf-8")
    (tmp_path / "build").mkdir()
    (tmp_path / "build" / mod.COMPILE_DB).write_text("[]", encoding="utf-8")
    assert mod._compile_db(tmp_path / "src" / "f.c", root=tmp_path) == db


def test_build_directory_is_the_fallback(tmp_path: Path) -> None:
    (tmp_path / "src").mkdir()
    (tmp_path / "build").mkdir()
    db = tmp_path / "build" / mod.COMPILE_DB
    db.write_text("[]", encoding="utf-8")
    assert mod._compile_db(tmp_path / "src" / "f.c", root=tmp_path) == db


def test_no_compile_database_anywhere_is_none(tmp_path: Path) -> None:
    (tmp_path / "src").mkdir()
    assert mod._compile_db(tmp_path / "src" / "f.c", root=tmp_path) is None


def test_format_analyzes_cuda_that_tidy_skips(tmp_path: Path, capsys) -> None:
    assert mod._analyzable("format", "k.cu", root=tmp_path) == (True, None)
    assert mod._analyzable("tidy", "k.cu", root=tmp_path) == (False, None)
    assert "not a host C/C++ translation unit" in capsys.readouterr().err


def test_tidy_skips_and_says_why_when_no_database_exists(
    tmp_path: Path, capsys, monkeypatch
) -> None:
    # Where the database is looked for is test_no_compile_database_anywhere_is_none's
    # business; this is about what happens once the answer comes back empty.
    monkeypatch.setattr(mod, "_compile_db", lambda path, root: None)
    analyzable, database = mod._analyzable("tidy", "f.cpp", root=tmp_path)
    assert (analyzable, database) == (False, None)
    assert "CMAKE_EXPORT_COMPILE_COMMANDS" in capsys.readouterr().err


def test_tidy_runs_once_a_database_exists(tmp_path: Path) -> None:
    db = tmp_path / mod.COMPILE_DB
    db.write_text("[]", encoding="utf-8")
    analyzable, database = mod._analyzable("tidy", "f.cpp", root=tmp_path)
    assert analyzable is True
    assert database == db


def test_tool_prefers_the_binary_beside_this_interpreter() -> None:
    beside = Path(sys.executable).parent / "clang-format"
    if not beside.is_file():
        pytest.skip("clang-format is not installed beside the interpreter")
    assert mod._tool("format") == str(beside)


def test_unchanged_file_is_not_analyzed(tmp_path: Path, monkeypatch) -> None:
    """An empty range list means "changed in mode or name only" -- skip it.

    Analyzing it whole is the exact regression the line scoping exists to stop.
    """
    monkeypatch.setattr(mod, "_ranges", lambda path, base, root: [])
    monkeypatch.setattr(
        mod, "_analyzable", lambda mode, path, root: (True, tmp_path / "db.json")
    )

    def explode(*_args, **_kwargs):  # pragma: no cover - must not be reached
        raise AssertionError("clang was invoked for a file with no changed lines")

    monkeypatch.setattr(mod.subprocess, "run", explode)
    assert (
        mod._check_one("format", "clang-format", "f.c", base="HEAD", root=tmp_path) == 0
    )
