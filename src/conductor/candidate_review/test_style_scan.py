"""Line scoping is the whole contract of this wrapper.

The rules themselves are tested in Rust, against the source they read. What is
tested here is the half that decides whether a finding reaches the operator: a
scope that is one line too wide red-gates a candidate for code it never touched,
and one that is too narrow passes the line it just wrote.
"""

from __future__ import annotations

import pytest

from conductor.candidate_review import style_scan
from conductor.candidate_review.diff_ranges import DiffError


def finding(path: str = "a.py", line: int = 10) -> dict:
    return {"path": path, "line": line, "rule": "dead/empty-function", "message": "m"}


def test_a_changed_range_includes_its_first_line():
    assert style_scan.in_range(4, ((4, 9),))


def test_a_changed_range_includes_its_last_line():
    assert style_scan.in_range(9, ((4, 9),))


def test_a_line_outside_every_range_is_not_in_scope():
    assert not style_scan.in_range(3, ((4, 9), (20, 22)))


def test_without_a_base_every_finding_is_kept(tmp_path):
    """A full-tree run has nothing to diff against, so nothing is scoped away."""
    rows = [finding(line=1), finding(line=900)]
    kept, unreadable = style_scan.scope(rows, "", tmp_path)
    assert kept == rows
    assert unreadable == []


def test_a_finding_on_an_unchanged_line_is_dropped(tmp_path, monkeypatch):
    monkeypatch.setattr(style_scan, "changed_line_ranges", lambda *a, **k: [(1, 2)])
    kept, _ = style_scan.scope([finding(line=10)], "HEAD", tmp_path)
    assert kept == []


def test_a_finding_on_a_changed_line_is_kept(tmp_path, monkeypatch):
    monkeypatch.setattr(style_scan, "changed_line_ranges", lambda *a, **k: [(9, 11)])
    row = finding(line=10)
    kept, _ = style_scan.scope([row], "HEAD", tmp_path)
    assert kept == [row]


def test_each_file_is_diffed_once_however_many_findings_it_has(tmp_path, monkeypatch):
    """`git diff` is a subprocess; running one per finding makes a noisy file the
    slowest thing in the gate for no added information."""
    calls: list[str] = []

    def record(path, *, base, cwd):
        calls.append(path)
        return [(1, 999)]

    monkeypatch.setattr(style_scan, "changed_line_ranges", record)
    rows = [finding(line=n) for n in (1, 2, 3)] + [finding(path="b.py", line=4)]
    style_scan.scope(rows, "HEAD", tmp_path)
    assert calls == ["a.py", "b.py"]


def test_an_undiffable_file_is_reported_rather_than_raised(tmp_path, monkeypatch):
    def explode(path, *, base, cwd):
        raise DiffError("git diff failed on a.py")

    monkeypatch.setattr(style_scan, "changed_line_ranges", explode)
    _, unreadable = style_scan.scope([finding()], "HEAD", tmp_path)
    assert unreadable == ["git diff failed on a.py"]


def test_only_python_files_reach_the_scanner(monkeypatch):
    """The gate hands this check every changed file in the candidate's classes."""
    seen: list[list[str]] = []
    monkeypatch.setattr(
        style_scan, "_scan", lambda paths, language: seen.append(paths) or []
    )
    style_scan.main(["a.py", "notes.md", "Makefile"])
    assert seen == [["a.py"]]


def test_a_candidate_with_no_python_never_builds_the_scanner(monkeypatch):
    def refuse(paths, language):
        raise AssertionError("the scanner must not run")

    monkeypatch.setattr(style_scan, "_scan", refuse)
    assert style_scan.main(["CHANGELOG"]) == 0


def test_version_answers_without_the_native_extension(monkeypatch, capsys):
    """The gate's tool preflight asks this before it decides to schedule the
    check, on a machine where the extension may not be built yet."""

    def refuse(paths, language):
        raise AssertionError("the scanner must not run")

    monkeypatch.setattr(style_scan, "_scan", refuse)
    assert style_scan.main(["--version"]) == 0
    assert "style-scan" in capsys.readouterr().out


def test_an_undiffable_file_fails_the_check(monkeypatch, tmp_path):
    """Scoping that cannot be computed is not scoping that found nothing."""
    monkeypatch.setattr(style_scan, "_scan", lambda paths, language: [finding()])
    monkeypatch.setattr(
        style_scan, "scope", lambda rows, base, root: ([], ["git diff failed"])
    )
    assert style_scan.main(["--base", "HEAD", "a.py"]) == 1


def test_a_surviving_finding_fails_the_check(monkeypatch):
    monkeypatch.setattr(style_scan, "_scan", lambda paths, language: [finding()])
    monkeypatch.setattr(style_scan, "scope", lambda rows, base, root: (rows, []))
    assert style_scan.main(["a.py"]) == 1


def test_findings_scoped_away_leave_the_check_passing(monkeypatch):
    monkeypatch.setattr(style_scan, "_scan", lambda paths, language: [finding()])
    monkeypatch.setattr(style_scan, "scope", lambda rows, base, root: ([], []))
    assert style_scan.main(["a.py"]) == 0


def test_findings_are_printed_where_the_gate_captures_them(monkeypatch, capsys):
    """The gate reads a failing command's stderr into the finding it reports; a
    finding printed to stdout is a red gate with no reason attached."""
    monkeypatch.setattr(style_scan, "_scan", lambda paths, language: [finding(line=7)])
    monkeypatch.setattr(style_scan, "scope", lambda rows, base, root: (rows, []))
    style_scan.main(["a.py"])
    captured = capsys.readouterr()
    assert "a.py:7" in captured.err
    assert captured.out == ""


def test_the_published_rule_list_matches_the_scanner(monkeypatch):
    """Two lists of rule names drift; this one refuses to."""
    slop_core = pytest.importorskip("slop_core")
    assert tuple(slop_core.style_scan_rules()) == style_scan.RULES[:-1]


def test_a_swallowed_error_is_reported_under_the_published_rule_name(tmp_path):
    """The fallback rule comes from a different scanner and a different parser,
    so nothing but an end-to-end scan proves its name is the published one."""
    pytest.importorskip("slop_core")
    source = tmp_path / "swallow.py"
    source.write_text("try:\n    risky()\nexcept ValueError:\n    pass\n")
    rules = {row["rule"] for row in style_scan._scan([str(source)], "python")}
    assert rules == {"failure/silent-fallback"}
    assert "failure/silent-fallback" in style_scan.RULES


def test_both_scanners_report_one_pass_down_the_file(tmp_path):
    """Concatenating two separately sorted lists reports the file twice over.

    The swallow is early and the empty function is late, so appending one
    scanner's rows to the other's comes out descending and only the sort
    rescues it.
    """
    pytest.importorskip("slop_core")
    source = tmp_path / "mixed.py"
    source.write_text(
        "def swallow():\n"
        "    try:\n"
        "        risky()\n"
        "    except ValueError:\n"
        "        pass\n"
        "\n"
        "\n"
        "def stub():\n"
        "    pass\n"
    )
    rows = style_scan._scan([str(source)], "python")
    assert [row["rule"] for row in rows] == [
        "failure/silent-fallback",
        "dead/empty-function",
    ]
    assert [row["line"] for row in rows] == [4, 8]


def test_only_rust_files_reach_the_rust_scanner(monkeypatch):
    """`rust` is a wider class than `.rs`: it carries Cargo.toml and the lockfile."""
    seen: list[list[str]] = []
    monkeypatch.setattr(
        style_scan, "_scan", lambda paths, language: seen.append(paths) or []
    )
    style_scan.main(["--language", "rust", "a.rs", "Cargo.toml", "Cargo.lock", "b.py"])
    assert seen == [["a.rs"]]


def test_the_language_reaches_the_scanner(monkeypatch):
    """One module drives two scanners; picking the wrong one reports nothing."""
    seen: list[str] = []
    monkeypatch.setattr(
        style_scan, "_scan", lambda paths, language: seen.append(language) or []
    )
    style_scan.main(["--language", "rust", "a.rs"])
    style_scan.main(["a.py"])
    assert seen == ["rust", "python"]


def test_the_published_rust_rule_list_matches_the_scanner():
    """Two lists of rule names drift; this one refuses to."""
    slop_core = pytest.importorskip("slop_core")
    assert tuple(sorted(slop_core.rust_scan_rules())) == style_scan.RUST_RULES


def test_a_production_unwrap_is_reported_under_the_published_rule_name(tmp_path):
    """The Rust rules come from a third scanner and a third grammar, so nothing
    but an end-to-end scan proves their names are the published ones."""
    pytest.importorskip("slop_core")
    source = tmp_path / "sample.rs"
    source.write_text("fn run(v: Vec<u32>) -> u32 {\n    *v.first().unwrap()\n}\n")
    rows = style_scan._scan([str(source)], "rust")
    assert [row["rule"] for row in rows] == ["failure/rust-unwrap"]
    assert "failure/rust-unwrap" in style_scan.RUST_RULES
