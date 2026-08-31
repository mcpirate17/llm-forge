"""The backlog's contract: items not findings, shipped first, and an honest diff."""

from __future__ import annotations

import json

import pytest

sl = pytest.importorskip(
    "conductor.slop_ledger",
    reason="slop_core not built; run `make -C research/runtime/native slop-core`",
    exc_type=ImportError,
)


def _f(module, qualname, verdict, rule="drop_raise_guard", lineno=10):
    return {
        "module": module,
        "qualname": qualname,
        "verdict": verdict,
        "rule": rule,
        "lineno": lineno,
        "description": f"{rule} removed",
    }


def test_findings_collapse_to_one_item_per_function_and_verdict():
    """The first real sweep: 2,776 findings over 931 functions. The findings are not
    the work."""
    items = sl.aggregate(
        [
            _f("conductor/a.py", "run", "NOT_EXERCISED", "drop_clamp", 40),
            _f("conductor/a.py", "run", "NOT_EXERCISED", "drop_detach", 12),
            _f("conductor/a.py", "run", "NOT_EXERCISED", "drop_where", 30),
        ]
    )
    assert len(items) == 1
    assert items[0]["findings"] == 3
    assert items[0]["rules"] == ["drop_clamp", "drop_detach", "drop_where"]
    # opens at the earliest line, not whichever finding arrived first
    assert items[0]["line"] == 12


def test_shipped_code_outranks_a_much_larger_pile_of_exploratory():
    """86% of the first sweep was research/tools one-off scripts. Ranked by volume the
    61 shipped items sat under 408 nobody should act on."""
    findings = [_f("conductor/a.py", "run", "NOT_EXERCISED")]
    findings += [
        _f("research/tools/x.py", "helper", "NOT_EXERCISED", f"r{i}") for i in range(30)
    ]
    items = sl.aggregate(findings)
    assert items[0]["tier"] == "shipped"
    assert items[0]["module"] == "conductor/a.py"
    assert items[1]["tier"] == "exploratory"
    assert items[1]["findings"] == 30


def test_research_tools_is_not_shipped_but_research_synthesis_is():
    # The distinction the whole report rests on.
    items = {
        i["module"]: i["tier"]
        for i in sl.aggregate(
            [
                _f("research/tools/x.py", "a", "NOT_EXERCISED"),
                _f("research/synthesis/y.py", "b", "NOT_EXERCISED"),
            ]
        )
    }
    assert items["research/tools/x.py"] == "exploratory"
    assert items["research/synthesis/y.py"] == "shipped"


def test_a_rerun_of_the_same_sweep_reports_nothing_new(tmp_path):
    ledger = tmp_path / "ledger.json"
    findings = [_f("conductor/a.py", "run", "NOT_EXERCISED")]
    items = sl.aggregate(findings)
    scope = ["conductor/a.py"]
    sl.save(items, scope, ledger)

    new, carried, fixed = sl.diff(sl.load(ledger), items, scope)
    assert new == []
    assert len(carried) == 1
    assert fixed == []


def test_fixed_counts_only_modules_this_sweep_actually_covered(tmp_path):
    """The trap. A narrower sweep must not report everything it skipped as fixed --
    the burndown would become a function of what you happened to scan."""
    ledger = tmp_path / "ledger.json"
    both = sl.aggregate(
        [
            _f("conductor/a.py", "run", "NOT_EXERCISED"),
            _f("conductor/b.py", "run", "NOT_EXERCISED"),
        ]
    )
    sl.save(both, ["conductor/a.py", "conductor/b.py"], ledger)

    # Now sweep only a.py, and find it clean.
    new, carried, fixed = sl.diff(sl.load(ledger), [], ["conductor/a.py"])
    assert [i["module"] for i in fixed] == ["conductor/a.py"]
    assert new == [] and carried == []


def test_a_narrow_sweep_does_not_erase_the_rest_of_the_backlog(tmp_path):
    ledger = tmp_path / "ledger.json"
    sl.save(
        sl.aggregate(
            [
                _f("conductor/a.py", "run", "NOT_EXERCISED"),
                _f("conductor/b.py", "run", "NOT_EXERCISED"),
            ]
        ),
        ["conductor/a.py", "conductor/b.py"],
        ledger,
    )

    sl.save(
        sl.aggregate([_f("conductor/a.py", "run", "NO_DIFFERENCE_OBSERVED")]),
        ["conductor/a.py"],
        ledger,
    )

    kept = {(i["module"], i["verdict"]) for i in sl.load(ledger)["items"]}
    assert ("conductor/b.py", "NOT_EXERCISED") in kept, "b.py's backlog was erased"
    assert ("conductor/a.py", "NO_DIFFERENCE_OBSERVED") in kept


def test_an_item_survives_the_file_being_edited_around_it():
    """Identity excludes the line number: an id that moved with edits would report one
    item fixed and another appearing every time a docstring grew."""
    a = sl.aggregate([_f("conductor/a.py", "run", "NOT_EXERCISED", lineno=10)])
    b = sl.aggregate([_f("conductor/a.py", "run", "NOT_EXERCISED", lineno=900)])
    assert a[0]["id"] == b[0]["id"]


def test_the_report_lists_shipped_items_and_only_counts_exploratory():
    items = sl.aggregate(
        [
            _f("conductor/a.py", "shipped_fn", "NOT_EXERCISED"),
            _f("research/tools/x.py", "throwaway_fn", "NOT_EXERCISED"),
        ]
    )
    report = sl.render(items, [], [], ["conductor/a.py", "research/tools/x.py"])
    assert "shipped_fn" in report
    assert "throwaway_fn" not in report, (
        "exploratory items should be counted, not listed"
    )
    assert "Exploratory (1 new)" in report


def test_the_report_says_so_when_nothing_changed():
    report = sl.render([], [], [], ["conductor/a.py"])
    assert "Nothing new and nothing fixed" in report


def test_the_cli_writes_nothing_on_a_dry_run(tmp_path, capsys):
    findings = tmp_path / "sweep.json"
    findings.write_text(
        json.dumps(
            {
                "blocking": [],
                "advisory": [_f("conductor/a.py", "run", "NO_DIFFERENCE_OBSERVED")],
                "untested": [],
                "modules_without_drivers": [],
            }
        )
    )
    ledger = tmp_path / "ledger.json"
    report = tmp_path / "report.md"
    assert (
        sl.main(
            [
                str(findings),
                "--ledger",
                str(ledger),
                "--report",
                str(report),
                "--dry-run",
            ]
        )
        == 0
    )
    assert "Slop backlog" in capsys.readouterr().out
    assert not ledger.exists() and not report.exists()


def test_the_cli_writes_both_artifacts(tmp_path):
    findings = tmp_path / "sweep.json"
    findings.write_text(
        json.dumps(
            {
                "blocking": [],
                "advisory": [],
                "untested": [_f("conductor/a.py", "run", "NOT_EXERCISED")],
                "modules_without_drivers": [],
            }
        )
    )
    ledger = tmp_path / "ledger.json"
    report = tmp_path / "report.md"
    assert (
        sl.main([str(findings), "--ledger", str(ledger), "--report", str(report)]) == 0
    )
    assert "run" in report.read_text()
    assert json.loads(ledger.read_text())["items"][0]["qualname"] == "run"


def test_every_bucket_of_a_summary_reaches_the_backlog():
    """Blocking, advisory and untested are three buckets of one report; a backlog that
    read only one of them would be silently partial."""
    summary = {
        "blocking": [_f("conductor/a.py", "b", "REACHABLE_BUT_UNTESTED")],
        "advisory": [_f("conductor/a.py", "c", "NO_DIFFERENCE_OBSERVED")],
        "untested": [_f("conductor/a.py", "d", "NOT_EXERCISED")],
    }
    assert len(sl.findings_from_summary(summary)) == 3


def test_a_missing_ledger_is_a_first_run_not_a_crash(tmp_path):
    empty = tmp_path / "nope.json"
    assert sl.load(empty)["items"] == []
    # a zero-byte file is the same case -- existence is not content
    stub = tmp_path / "stub.json"
    stub.write_text("")
    assert sl.load(stub)["items"] == []


def test_the_ledger_scope_covers_every_module_it_speaks_for(tmp_path):
    """`scope` is what the ledger covers, not what the last sweep touched.

    Recording the sweep's scope made a two-module run overwrite the record of a
    164-module one: the file claimed to cover 1 module while holding 1,543 items from
    164. `last_sweep_scope` keeps the narrower fact, which is the one `diff` needs.
    """
    ledger = tmp_path / "ledger.json"
    sl.save(sl.aggregate([
        _f("conductor/a.py", "run", "NOT_EXERCISED"),
        _f("conductor/b.py", "run", "NOT_EXERCISED"),
    ]), ["conductor/a.py", "conductor/b.py"], ledger)

    sl.save(sl.aggregate([_f("conductor/a.py", "run", "NOT_EXERCISED")]),
            ["conductor/a.py"], ledger)

    written = json.loads(ledger.read_text())
    assert written["scope"] == ["conductor/a.py", "conductor/b.py"]
    assert written["last_sweep_scope"] == ["conductor/a.py"]
