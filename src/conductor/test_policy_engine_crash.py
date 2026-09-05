"""A crashed check and an unrecordable summary must both stay legible.

Both defects here were found the same way on 2026-09-05: `make gate` reported
`CRITICAL equivalence-probe/policy-engine-crash: TypeError: Object of type bytes
is not JSON serializable` and nothing else. No file, no line, no frames -- because
`_crash_result` formatted the exception and threw the traceback away, and because
the `json.dumps` that raised it sat inside a function whose own docstring called
itself best effort while catching only `OSError`.

The tests are in their own module rather than appended to
`conductor/test_candidate_review.py` because the one line covering `_crash_result`
there asserts `.status == CheckStatus.ERROR` and nothing more, inside a test that
also covers engine integrity, bypass evidence and tree integrity. A test that
cannot fail for the reason you care about is not coverage.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

from conductor.candidate_review import engine as review_engine
from conductor.candidate_review import equivalence_probe_check as probe_check
from conductor.candidate_review.model import CheckStatus, Severity


def _raised(exc_type: type[BaseException], message: str) -> BaseException:
    """Return an exception carrying a real `__traceback__`, as `future.result()` does."""

    def inner() -> None:
        raise exc_type(message)

    def outer() -> None:
        inner()

    try:
        outer()
    except BaseException as exc:  # noqa: BLE001 - the point is to capture any raise
        return exc
    raise AssertionError("outer() did not raise")


def test_a_crashed_check_names_the_line_it_raised_on() -> None:
    """The message must locate the raise, not merely restate the exception."""
    exc = _raised(TypeError, "Object of type bytes is not JSON serializable")
    result = review_engine._crash_result("equivalence-probe", exc)

    assert result.status == CheckStatus.ERROR
    finding = result.findings[0]
    assert finding.severity == Severity.CRITICAL
    assert "TypeError: Object of type bytes is not JSON serializable" in finding.message
    assert "test_policy_engine_crash.py:" in finding.message, (
        "the raise site is the whole point; without it this cost three rounds of bisection"
    )
    assert "in inner" in finding.message, "the deepest frame is the one that raised"


def test_the_frames_reach_the_receipt_even_though_the_message_holds_one_line() -> None:
    """`evidence` carries the stack: the terminal gets a line, the receipt gets the trace."""
    exc = _raised(RuntimeError, "boom")
    finding = review_engine._crash_result("probe", exc).findings[0]

    trace = finding.evidence["traceback"]
    assert "RuntimeError: boom" in trace
    assert "in outer" in trace and "in inner" in trace, "both frames, not just the last"
    assert len(trace) <= review_engine.CRASH_TRACEBACK_CHARS


def test_a_long_traceback_is_truncated_to_the_frames_that_name_the_defect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unbounded trace lands in every stored receipt; the tail is what identifies it.

    The cap is lowered rather than the stack deepened because `format_exception`
    folds a recursive stack into `[Previous line repeated N more times]` -- 300
    frames render in under a kilobyte, so the obvious way to write this test
    passes against a `_crash_result` with no bound at all.
    """
    monkeypatch.setattr(review_engine, "CRASH_TRACEBACK_CHARS", 40)
    finding = review_engine._crash_result(
        "probe", _raised(RuntimeError, "boom")
    ).findings[0]

    trace = finding.evidence["traceback"]
    assert len(trace) == 40, "the bound is applied, not merely declared"
    assert trace.endswith("RuntimeError: boom\n"), "the tail is kept, not the head"


def test_a_crash_finding_carries_no_path_so_it_can_never_read_as_inherited() -> None:
    """A finding naming a path outside the candidate's diff is inherited, and never blocks.

    A crash must always block, so it must not carry a path -- the raise site goes
    in the message instead. This is the assertion that stops someone from
    "improving" the finding by populating `path` with the traceback's filename.
    """
    finding = review_engine._crash_result("probe", _raised(RuntimeError, "x")).findings[
        0
    ]
    assert finding.path is None
    assert finding.line is None


def test_an_unserializable_summary_is_reported_not_fatal(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture,
) -> None:
    """`json.dumps` raises TypeError, not OSError -- and this function is best-effort.

    On 2026-09-05 a `bytes` `stderr_tail` from `subprocess.TimeoutExpired` reached
    here and took the whole code review down from inside the bookkeeping.
    """
    from conductor import slop_ledger

    monkeypatch.setattr(slop_ledger, "GATE_FINDINGS", tmp_path / "gate_findings")
    summary = {"incomplete": [{"stderr_tail": b"killed mid-import"}]}

    probe_check._record_for_backlog(summary, None)

    assert "not recorded" in capsys.readouterr().err, "caught, but never silent"
    assert list((tmp_path / "gate_findings").glob("*.json")) == []
    assert list((tmp_path / "gate_findings").glob("*.part")) == [], (
        "a partial write must not be left where the backlog reader will find it"
    )


def test_the_crash_help_names_a_flag_the_gate_actually_accepts() -> None:
    """The remediation line told operators to run `--only`, which has never existed.

    Binding the help text to the real parser is what stops it drifting again: the
    flag is read out of the message and handed to `slop_gate` itself.
    """
    source = pathlib.Path(probe_check.__file__).read_text(encoding="utf-8")
    assert "python -m conductor.slop_gate --module <module>" in source
    assert "--only" not in source

    completed = subprocess.run(
        [sys.executable, "-m", "conductor.slop_gate", "--help"],
        capture_output=True,
        text=True,
        check=True,
    )
    assert "--module" in completed.stdout
    assert "--only" not in completed.stdout
