"""A scan that could not read a file must say so, and say what the gap costs.

The three checks here walk the candidate snapshot off the filesystem, and each
one used to answer an unreadable file with a bare `continue`. The result was a
PASS indistinguishable from a scan that read everything: nothing in the receipt
recorded that a file had been skipped, so the check's coverage quietly became a
function of which files happened to open. These fixtures pin both halves of the
repair -- the counts that ride out on every run, and the asymmetry between a
skipped file the check was supposed to adjudicate (fatal to the verdict) and a
skipped file elsewhere in the comparison corpus (advisory, because it can only
cost a true finding, never invent a false one).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review.checks import (
    ReviewContext,
    check_duplicate_function_bodies,
    check_native_source,
)
from conductor.candidate_review.model import Candidate, CheckResult, Severity
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.policy_path import resolve_policy_path
from conductor.candidate_review.quality_checks import check_structure_audit
from conductor.test_candidate_review import _change

POLICY = load_policy(resolve_policy_path())

# Not valid UTF-8 in any position, so read_text raises UnicodeDecodeError. A
# chmod would be the other way to make a file unreadable, but it does nothing
# when the suite runs as root, and a test that silently stops testing is the
# exact failure this module exists to catch.
UNREADABLE = b"\xff\xfe\x00 not utf-8"


def _context(
    tmp_path: Path,
    files: dict[str, str | bytes],
    changed: tuple[str, ...],
    classes: tuple[str, ...] = ("python", "source"),
) -> ReviewContext:
    snapshot = tmp_path / "snapshot"
    for rel, body in files.items():
        path = snapshot / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(body, bytes):
            path.write_bytes(body)
        else:
            path.write_text(body, encoding="utf-8")
    return ReviewContext(
        repo=tmp_path / "repo",
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=tuple(_change(rel, classes=classes) for rel in changed),
        ),
        entries=(),
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )


def _rules(result: CheckResult) -> dict[str, Severity]:
    return {finding.rule_id: finding.severity for finding in result.findings}


CLEAN = "def alpha() -> int:\n    return 1\n"


def test_structure_audit_fails_closed_on_an_unreadable_changed_file(
    tmp_path: Path,
) -> None:
    """The audit cannot pass over source it never opened."""

    ctx = _context(
        tmp_path,
        {"pkg/good.py": CLEAN, "pkg/broken.py": UNREADABLE},
        ("pkg/good.py", "pkg/broken.py"),
    )
    result = check_structure_audit(ctx)
    assert _rules(result)["unreadable-changed-file"] is Severity.HIGH
    assert result.metrics["files_skipped"] == 1
    assert "pkg/broken.py" in result.metrics["skipped_files"]


def test_an_unreadable_file_outside_the_change_set_stays_advisory(
    tmp_path: Path,
) -> None:
    """A bad byte in a file nobody touched must not veto every candidate.

    The corpus half of the scan only supplies comparison material, so losing a
    file there can cost a true finding but cannot manufacture a false one. If it
    blocked, one unreadable file anywhere in the tree would fail the repo.
    """

    ctx = _context(
        tmp_path,
        {"pkg/good.py": CLEAN, "vendor/broken.py": UNREADABLE},
        ("pkg/good.py",),
    )
    result = check_structure_audit(ctx)
    rules = _rules(result)
    assert rules["incomplete-scan"] is Severity.MEDIUM
    assert "unreadable-changed-file" not in rules
    assert result.metrics["files_skipped"] == 1


def test_a_complete_scan_still_reports_its_coverage(tmp_path: Path) -> None:
    """Zero skips is evidence only if the field is present when nothing failed."""

    ctx = _context(tmp_path, {"pkg/good.py": CLEAN}, ("pkg/good.py",))
    result = check_structure_audit(ctx)
    assert result.metrics["files_skipped"] == 0
    assert result.metrics["files_read"] == result.metrics["files_expected"] == 1
    assert result.metrics["skipped_files"] == {}
    assert "incomplete-scan" not in _rules(result)


def test_native_source_fails_closed_at_critical_on_an_unreadable_file(
    tmp_path: Path,
) -> None:
    """Every file this check reads is one the candidate changed.

    So a skip is not a corpus gap, it is a CRITICAL unsafe-API scan reporting
    PASS over native source that nothing examined.
    """

    ctx = _context(
        tmp_path,
        {"src/a.c": "int main(void) { return 0; }\n", "src/b.c": UNREADABLE},
        ("src/a.c", "src/b.c"),
        classes=("native", "source"),
    )
    result = check_native_source(ctx)
    assert _rules(result)["unreadable-changed-file"] is Severity.CRITICAL
    assert result.metrics["files_skipped"] == 1


def test_native_source_still_flags_the_unsafe_api_it_can_read(
    tmp_path: Path,
) -> None:
    """The control: routing reads through the ledger must not cost a detection."""

    ctx = _context(
        tmp_path,
        {"src/a.c": "void f(char *d, char *s) { strcpy(d, s); }\n"},
        ("src/a.c",),
        classes=("native", "source"),
    )
    result = check_native_source(ctx)
    assert _rules(result)["unsafe-native-api"] is Severity.CRITICAL
    assert result.metrics["files_skipped"] == 0
    assert result.metrics["files_read"] == 1


def test_duplicate_bodies_fails_closed_on_an_unreadable_changed_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A changed file that will not open cannot be cleared of duplicating anything.

    changed_line_numbers shells out to git against a real repository, which this
    fixture has no need of: an empty map means "no changed lines identified",
    which suppresses duplicate findings entirely and so cannot be what produces
    the finding asserted below.
    """

    monkeypatch.setattr(
        "conductor.candidate_review.checks.changed_line_numbers",
        lambda *args, **kwargs: {},
    )
    ctx = _context(
        tmp_path,
        {"pkg/good.py": CLEAN, "pkg/broken.py": UNREADABLE},
        ("pkg/good.py", "pkg/broken.py"),
    )
    result = check_duplicate_function_bodies(ctx)
    assert _rules(result)["unreadable-changed-file"] is Severity.HIGH
    assert result.metrics["files_skipped"] == 1
