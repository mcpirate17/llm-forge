from __future__ import annotations

import io
from pathlib import Path

import pytest
from pylint.checkers.symilar import Symilar

from conductor.guardrail_duplicates import _IndexedSymilar, scan_duplicates


def _result(checker, sources):
    for name, source in sources:
        checker.append_stream(name, io.StringIO(source))
    return sorted(
        (count, sorted((item.name, start, end) for item, start, end in entries))
        for count, entries in checker._compute_sims()
    )


def _checker(kind):
    return kind(
        min_lines=10,
        ignore_comments=True,
        ignore_docstrings=True,
        ignore_imports=True,
        ignore_signatures=True,
    )


def _body(count=16):
    return "".join(f"    value_{number} = {number}\n" for number in range(count))


def test_indexed_matches_pylint_across_directories_and_ignored_lines():
    body = _body()
    sources = [
        ("first/a.py", 'import os\ndef first():\n    """first doc"""\n' + body),
        (
            "unrelated/z.py",
            "def unrelated():\n" + "".join(f"    other_{i} = {i}\n" for i in range(16)),
        ),
        (
            "distant/b.py",
            'import sys\ndef second(argument):\n    """other doc"""\n# comment\n'
            + body,
        ),
    ]
    indexed = _checker(_IndexedSymilar)
    actual = _result(indexed, sources)
    assert actual == _result(_checker(Symilar), sources)
    assert len(actual) == 1
    assert {entry[0] for entry in actual[0][1]} == {"first/a.py", "distant/b.py"}
    assert indexed.candidate_count == 1


@pytest.mark.parametrize("count", [9, 10, 11, 25])
def test_threshold_and_repeated_windows_match_pylint(count):
    sources = [
        ("a.py", "def a():\n" + _body(count)),
        ("b.py", "def b():\n" + _body(count)),
    ]
    assert _result(_checker(_IndexedSymilar), sources) == _result(
        _checker(Symilar), sources
    )


def test_global_index_has_no_pairs_for_unrelated_files():
    sources = [
        (
            f"source_{file}.py",
            "".join(f"value_{file}_{line} = {line}\n" for line in range(15)),
        )
        for file in range(200)
    ]
    indexed = _checker(_IndexedSymilar)
    assert _result(indexed, sources) == []
    assert indexed.candidate_count == 0
    assert indexed.indexed_windows == 1200


def test_invalid_source_and_encoding_are_errors_not_empty_results(tmp_path: Path):
    (tmp_path / "syntax.py").write_text("def broken(:\n")
    with pytest.raises(ValueError, match="cannot normalize syntax.py"):
        scan_duplicates(tmp_path, ["syntax.py"])
    (tmp_path / "bytes.py").write_bytes(b"\xff\xff\n")
    with pytest.raises(ValueError, match="cannot normalize bytes.py"):
        scan_duplicates(tmp_path, ["bytes.py"])


def test_scan_reports_global_counts_and_source_locations(tmp_path: Path):
    for name in ["a.py", "b.py"]:
        (tmp_path / name).write_text("def function():\n" + _body())
    result = scan_duplicates(tmp_path, ["b.py", "a.py", "a.py"])
    assert result.files == 2
    assert result.possible_pairs == result.candidate_pairs == 1
    assert len(result.findings) == 1
    assert "a.py:2-17" in result.findings[0]
    assert "b.py:2-17" in result.findings[0]
    assert result.elapsed_seconds >= 0


@pytest.mark.parametrize("directive", ["disable=duplicate-code", "skip-file"])
def test_source_suppression_is_not_lost_by_native_candidate_selection(
    tmp_path: Path, directive: str
):
    (tmp_path / "a.py").write_text("def first():\n" + _body())
    (tmp_path / "b.py").write_text(f"# pylint: {directive}\ndef second():\n" + _body())
    result = scan_duplicates(tmp_path, ["a.py", "b.py"])
    assert result.findings == ()
    assert result.candidate_pairs == 0


def test_scoped_suppression_respects_reenable(tmp_path: Path):
    (tmp_path / "a.py").write_text("def first():\n" + _body())
    (tmp_path / "b.py").write_text(
        "# pylint: disable=duplicate-code\nignored = True\n"
        "# pylint: enable=duplicate-code\ndef second():\n" + _body()
    )
    assert len(scan_duplicates(tmp_path, ["a.py", "b.py"]).findings) == 1


@pytest.mark.parametrize("config_name", [".pylintrc", "pyproject.toml"])
def test_host_normalization_configuration_is_preserved(
    tmp_path: Path, config_name: str
):
    if config_name == ".pylintrc":
        config = "[SIMILARITIES]\nignore-imports=no\nmin-similarity-lines=100\n"
    else:
        config = "[tool.pylint.similarities]\nignore-imports=false\nmin-similarity-lines=100\n"
    (tmp_path / config_name).write_text(config)
    source = "".join(f"import module_{index}\n" for index in range(15))
    for name in ("a.py", "b.py"):
        (tmp_path / name).write_text(source)
    result = scan_duplicates(tmp_path, ["a.py", "b.py"])
    # Imports remain eligible, but the audit's explicit min-lines=10 override
    # still wins over the host's min-lines=100, matching the previous CLI.
    assert result.normalization["ignore_imports"] is False
    assert len(result.findings) == 1


def test_all_host_ignore_options_are_resolved(tmp_path: Path):
    from conductor.guardrail_duplicates import _configured_options

    (tmp_path / ".pylintrc").write_text(
        "[SIMILARITIES]\nignore-comments=no\nignore-docstrings=no\n"
        "ignore-imports=yes\nignore-signatures=no\n"
    )
    options = _configured_options(tmp_path)
    assert not options.ignore_comments
    assert not options.ignore_docstrings
    assert options.ignore_imports
    assert not options.ignore_signatures
