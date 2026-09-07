"""Contracts for the cargo-mutants adapter -- the Rust half.

Two of cargo-mutants' behaviours would silently corrupt a verdict if the adapter
took them at face value: it exits non-zero whenever any mutant survived, and it
reports mutants that never compiled alongside ones the tests actually ran. Both
are pinned here.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.mutation_engine_cargo import (
    _engine_argv,
    _require_baseline,
    _rows,
)
from conductor.mutation_engine_generated import load_generated_campaign
from conductor.mutation_scope import CampaignError

REPO_ROOT = Path(__file__).resolve().parents[1]
CRATE = "tooling/native/snapshot-retention"


def outcome(
    summary: str,
    *,
    genre: str = "FnValue",
    replacement: str = "Ok(vec![])",
    function: str = "snapshot_stale_branches",
    line: int = 437,
    column: int = 5,
    path: str = "src/lib.rs",
) -> dict[str, object]:
    """One cargo-mutants outcome record, in the shape outcomes.json emits."""

    return {
        "scenario": {
            "Mutant": {
                "name": f"{path}:{line}:{column}: replace {function}",
                "package": "snapshot-retention",
                "file": path,
                "function": {
                    "function_name": function,
                    "return_type": "-> Result<Vec<SnapshotAction>, Error>",
                    "span": {
                        "start": {"line": line, "column": 1},
                        "end": {"line": line + 9, "column": 2},
                    },
                },
                "span": {
                    "start": {"line": line, "column": column},
                    "end": {"line": line + 1, "column": 40},
                },
                "replacement": replacement,
                "genre": genre,
            }
        },
        "summary": summary,
        "phase_results": [
            {"phase": "Build", "duration": 1.5},
            {"phase": "Test", "duration": 0.25},
        ],
    }


def baseline(summary: str = "Success") -> dict[str, object]:
    """The unmutated scenario cargo-mutants records alongside the mutants."""

    return {"scenario": "Baseline", "summary": summary, "phase_results": []}


def campaign(tmp_path: Path | None = None):
    """The committed Rust campaign."""

    return load_generated_campaign(
        REPO_ROOT
        / "conductor/mutation_campaigns/claude_snapshot_retention_cargo_20260906.json"
    )


def test_a_mutant_that_never_compiled_is_not_a_kill() -> None:
    """`Unviable` means the mutant never built, so no test could have caught it.

    cargo-mutants reported 24 of them on the trial crate. Folding those into the
    caught count -- which any plain kill-fraction does -- would have credited the
    suite with 24 detections it never made.
    """

    report = {
        "outcomes": [
            baseline(),
            outcome("Unviable", replacement="Default::default()"),
            outcome("CaughtMutant"),
        ]
    }
    rows = _rows(report, CRATE)
    outcomes = {row["outcome"] for row in rows}
    assert outcomes == {"UNVIABLE", "KILLED"}
    assert len(rows) == 2, "the baseline scenario is not a mutant"


def test_rows_carry_the_crate_relative_path_not_the_cargo_one() -> None:
    """cargo-mutants reports `src/lib.rs`; a receipt has to say which crate."""

    rows = _rows({"outcomes": [baseline(), outcome("MissedMutant")]}, CRATE)
    assert rows[0]["path"] == f"{CRATE}/src/lib.rs"
    assert rows[0]["outcome"] == "SURVIVED"
    assert rows[0]["function"] == "snapshot_stale_branches"


def test_mutants_are_ordered_and_named_independently_of_their_line() -> None:
    """Naming by line would churn the baseline whenever the file above moves."""

    early = _rows({"outcomes": [outcome("MissedMutant", line=100)]}, CRATE)
    late = _rows({"outcomes": [outcome("MissedMutant", line=900)]}, CRATE)
    assert early[0]["id"] == late[0]["id"]

    two = _rows(
        {
            "outcomes": [
                outcome("CaughtMutant", line=800, replacement="Ok(vec![])"),
                outcome("MissedMutant", line=100, replacement="Ok(vec![])"),
            ]
        },
        CRATE,
    )
    assert [row["line"] for row in two] == [100, 800], "sorted by position"
    assert len({row["id"] for row in two}) == 2


def test_an_unknown_summary_is_refused_not_guessed() -> None:
    """A cargo-mutants release that adds a summary must stop the run."""

    with pytest.raises(CampaignError, match="unknown summary"):
        _rows({"outcomes": [outcome("Flaky")]}, CRATE)


def test_a_scenario_that_is_neither_baseline_nor_mutant_is_refused() -> None:
    """The JSON schema is explicitly unstable across cargo-mutants releases."""

    with pytest.raises(CampaignError, match="unknown scenario"):
        _rows(
            {"outcomes": [{"scenario": {"Something": {}}, "summary": "Success"}]}, CRATE
        )


def test_a_red_unmutated_baseline_stops_the_run() -> None:
    """If the clean tree already fails, every mutant after it reads as caught."""

    with pytest.raises(CampaignError, match="baseline failed"):
        _require_baseline({"outcomes": [baseline("Failure"), outcome("CaughtMutant")]})
    with pytest.raises(CampaignError, match="no baseline scenario"):
        _require_baseline({"outcomes": [outcome("CaughtMutant")]})
    _require_baseline({"outcomes": [baseline(), outcome("CaughtMutant")]})


def test_the_invocation_pins_every_bound_from_the_manifest() -> None:
    """Nothing about the run may be left to whatever the tool defaults to.

    Every bound is asserted by value, not by presence: a wrong `--jobs` or a
    `--package` that names the wrong crate still produces a plausible-looking
    run, and dropping `--package` entirely mutates whatever else the workspace
    happens to contain.
    """

    subject = campaign()
    subject.exclude = ("src/generated/**",)
    argv = _engine_argv(subject, "/bin/cargo-mutants", Path("/out"))

    assert argv[:2] == ["/bin/cargo-mutants", "mutants"]
    assert argv[argv.index("--output") + 1] == "/out"
    assert argv[argv.index("--jobs") + 1] == str(subject.jobs)
    assert argv[argv.index("--timeout") + 1] == str(subject.mutant_timeout_seconds)
    assert argv[argv.index("--package") + 1] == "snapshot-retention"
    assert argv[argv.index("--manifest-path") + 1].endswith("Cargo.toml")

    # Every glob reaches the tool: a dropped one silently narrows the corpus.
    assert [argv[i + 1] for i, a in enumerate(argv) if a == "--file"] == list(
        subject.source
    )
    assert [argv[i + 1] for i, a in enumerate(argv) if a == "--exclude"] == [
        "src/generated/**"
    ]


def test_a_crate_without_a_package_is_not_narrowed_to_one() -> None:
    """`--package` is omitted rather than passed empty when none is named."""

    subject = campaign()
    subject.options = {"manifest_path": "x/Cargo.toml"}
    argv = _engine_argv(subject, "/bin/cargo-mutants", Path("/out"))
    assert "--package" not in argv


def test_every_receipt_field_a_row_carries_is_pinned() -> None:
    """The row keys are the receipt's schema; renaming one breaks the ratchet.

    The survivor baseline is matched on `id`, and `path`/`line`/`operator` are
    what make a survivor readable. None of them are checked by anything else,
    so a rename here would land silently.
    """

    (row,) = _rows({"outcomes": [outcome("MissedMutant")]}, CRATE)
    assert set(row) == {
        "id",
        "outcome",
        "path",
        "line",
        "operator",
        "original_text",
        "mutated_text",
        "function",
        "package",
        "duration_seconds",
    }
    assert row["line"] == 437
    assert row["operator"] == "FnValue"
    assert row["package"] == "snapshot-retention"
    assert row["mutated_text"] == "Ok(vec![])"
    # cargo-mutants publishes no original text, so the function identifies it.
    assert row["original_text"] == (
        "snapshot_stale_branches-> Result<Vec<SnapshotAction>, Error>"
    )
    # Build and test phases both count toward the recorded cost.
    assert row["duration_seconds"] == 1.75


def test_a_mutant_outside_any_function_is_still_named() -> None:
    """cargo-mutants mutates file-level constants too, and reports no function.

    Falling back to an empty name would collide every such mutant in a file
    into one identifier and silently merge them in the survivor baseline.
    """

    record = outcome("MissedMutant")
    del record["scenario"]["Mutant"]["function"]  # type: ignore[index]
    (row,) = _rows({"outcomes": [record]}, CRATE)
    assert row["function"] is None
    assert row["original_text"] == "<file>"
    assert row["id"]


def test_a_duration_is_recorded_to_microseconds() -> None:
    """The recorded precision is the receipt's contract, so it is pinned."""

    record = outcome("CaughtMutant")
    record["phase_results"] = [{"phase": "Build", "duration": 1.234_567_8}]  # type: ignore[index]
    assert _rows({"outcomes": [record]}, CRATE)[0]["duration_seconds"] == 1.234568


def test_a_crate_without_a_manifest_path_is_refused(tmp_path: Path) -> None:
    """cargo-mutants would otherwise mutate whatever crate the cwd resolves to."""

    subject = campaign()
    subject.options = {}
    with pytest.raises(CampaignError, match="manifest_path"):
        _engine_argv(subject, "/bin/cargo-mutants", Path("/out"))


def test_the_campaign_under_test_is_wired_end_to_end() -> None:
    """The manifest this adapter was proven on is loadable and pins a real file."""

    loaded = campaign()
    assert loaded.mutation_engine == "cargo-mutants"
    assert loaded.language == "rust"
    assert loaded.survivor_baseline, "the recorded survivor baseline must not be empty"
    for relative in loaded.source_sha256:
        assert (REPO_ROOT / relative).is_file(), relative


def test_parallel_workers_may_not_share_one_cargo_build_directory() -> None:
    """A shared CARGO_TARGET_DIR makes the same corpus classify differently.

    Measured: three runs of the same 196 mutants of `snapshot-retention` at
    jobs=4 with a shared target dir gave 45/43/46 survivors and 24/21/22
    unviable. Whether a mutant compiles cannot legitimately vary, and a corpus
    that jitters cannot carry a ratchet.
    """

    subject = campaign()
    subject.environment = {"CARGO_TARGET_DIR": "/home/tim/.cargo/shared"}
    subject.jobs = 4
    with pytest.raises(CampaignError, match="share one"):
        _engine_argv(subject, "/bin/cargo-mutants", Path("/out"))

    # One worker cannot race itself, so the same pin is allowed there.
    subject.jobs = 1
    _engine_argv(subject, "/bin/cargo-mutants", Path("/out"))


def test_the_committed_campaign_does_not_pin_a_shared_target_dir() -> None:
    """The manifest that ships must be the one that measures reproducibly."""

    subject = campaign()
    assert subject.jobs > 1
    assert "CARGO_TARGET_DIR" not in subject.environment
