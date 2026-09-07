"""Contracts for the Mull adapter -- the C and C++ half.

Everything engine-agnostic (naming, scoring, refusals) is proven in
`test_mutation_engine_generated`. What is left here is the handful of Mull
behaviours that turn a run into a number that looks fine and means nothing:
the millisecond timeout, the timeout floor, and above all the coverage filter
without which the runner reports unreached code as survived.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from conductor.mutation_engine_mull import (
    _configure_argv,
    _llvm_version,
    _engine_argv,
    _executables,
    _in_scope,
    _merge,
    _rows,
    _slice,
)
from conductor.mutation_engine_generated import load_generated_campaign
from conductor.mutation_scope import CampaignError

REPO_ROOT = Path(__file__).resolve().parents[1]
WORKTREE = REPO_ROOT
MANIFEST = "conductor/mutation_campaigns/claude_aria_kernels_mull_20260906.json"
SOURCE = "aria_core/src/cpu/norm.cpp"


def mutant(
    *,
    mutator: str = "cxx_lt_to_le",
    replacement: str = "<=",
    status: str = "Survived",
    line: int = 2,
    column: int = 12,
    end_line: int | None = None,
    end_column: int = 13,
    identifier: str | None = None,
) -> dict[str, object]:
    """One Mull mutant record, in the shape the Elements report emits."""

    return {
        "id": identifier or f"{mutator}:{SOURCE}:{line}:{column}",
        "mutatorName": mutator,
        "replacement": replacement,
        "location": {
            "start": {"line": line, "column": column},
            "end": {"line": end_line or line, "column": end_column},
        },
        "status": status,
    }


SOURCE_TEXT = (
    "void f(int n) {\n  for (int i = 0; i < n; ++i) {\n    g(i * 2);\n  }\n}\n"
)


def report(
    *mutants: dict[str, object], path: str = SOURCE, source: str = SOURCE_TEXT
) -> dict[str, object]:
    """One Elements report over a single file."""

    return {
        "files": {
            str(WORKTREE / path): {
                "language": "cpp",
                "source": source,
                "mutants": list(mutants),
            }
        }
    }


def campaign():
    """The committed C++ campaign."""

    return load_generated_campaign(REPO_ROOT / MANIFEST)


def test_the_mutant_timeout_is_converted_from_seconds_to_milliseconds() -> None:
    """Every other engine here takes seconds; Mull's `--timeout` is milliseconds.

    Passing the manifest's seconds straight through would set a 2ms budget and
    record the whole corpus as timed out -- which scores as neither killed nor
    survived, so the campaign would come back green over nothing.
    """

    subject = campaign()
    subject.mutant_timeout_seconds = 7
    argv = _engine_argv(
        subject,
        "/bin/mull-runner-18",
        Path("/b/t"),
        Path("/b/t.profdata"),
        Path("/b/r"),
        "t",
    )
    assert argv[argv.index("--timeout") + 1] == "7000"


def test_the_timeout_floor_is_pinned_because_these_suites_run_in_microseconds() -> None:
    """Mull's effective budget is max(baseline * 10, minimum-timeout).

    test_kernels finishes in under 10ms, so `baseline * 10` truncates to zero
    and every mutant that does any work is recorded as a timeout: 309 of them
    on the first run of this suite. The floor is manifest data, never defaulted.
    """

    subject = campaign()
    subject.mutant_timeout_seconds = 7
    argv = _engine_argv(
        subject,
        "/bin/mull-runner-18",
        Path("/b/t"),
        Path("/b/t.profdata"),
        Path("/b/r"),
        "t",
    )
    assert argv[argv.index("--minimum-timeout") + 1] == "7000"


def test_the_run_is_always_filtered_by_real_coverage_data() -> None:
    """Without profdata Mull cannot tell unreached code from undefended code.

    Measured: this suite scored {Survived: 4616, Timeout: 309, Killed: 1} with
    `--include-not-covered` and no `--coverage-info`, and 340 killed of 546 with
    the profiles from both binaries. The first number is not a finding, it is a
    broken instrument, so the flag that produced it is never passed.
    """

    argv = _engine_argv(
        campaign(),
        "/bin/mull-runner-18",
        Path("/b/t"),
        Path("/b/t.profdata"),
        Path("/b/r"),
        "t",
    )
    assert argv[argv.index("--coverage-info") + 1] == "/b/t.profdata"
    assert "--include-not-covered" not in argv


def test_the_invocation_pins_every_other_bound_from_the_manifest() -> None:
    """Nothing about the run may be left to whatever the tool defaults to."""

    subject = campaign()
    subject.jobs = 3
    subject.exclude = ("cxx_remove_void_call",)
    argv = _engine_argv(
        subject,
        "/bin/mull-runner-18",
        Path("/b/t"),
        Path("/b/t.profdata"),
        Path("/b/reports"),
        "kernels",
    )
    assert argv[:2] == ["/bin/mull-runner-18", "/b/t"]
    assert argv[argv.index("--workers") + 1] == "3"
    assert argv[argv.index("--reporters") + 1] == "Elements"
    assert argv[argv.index("--report-dir") + 1] == "/b/reports"
    assert argv[argv.index("--report-name") + 1] == "kernels"
    assert argv[argv.index("--ignore-mutators") + 1] == "cxx_remove_void_call"


def test_the_build_carries_the_pass_plugin_and_both_instrumentations() -> None:
    """One build is the corpus and the coverage map; a missing flag loses one.

    Without `-fpass-plugin` the binary holds no mutants at all. Without
    `-fprofile-instr-generate` and `-fcoverage-mapping` it produces no profile,
    and the coverage filter above then has nothing to filter with.
    """

    subject = campaign()
    argv = _configure_argv(subject, WORKTREE, Path("/b"), "18")
    flags = [a for a in argv if a.startswith("-DCMAKE_CXX_FLAGS=")]
    assert flags, "the C++ flags must be set explicitly"
    assert "-fpass-plugin=/usr/lib/mull-ir-frontend-18" in flags[0]
    assert "-fprofile-instr-generate" in flags[0]
    assert "-fcoverage-mapping" in flags[0]
    # The profile runtime has to be linked, or the binary runs and writes no
    # .profraw and the whole coverage step silently produces nothing.
    assert any(
        a.startswith("-DCMAKE_EXE_LINKER_FLAGS=") and "-fprofile-instr-generate" in a
        for a in argv
    )
    assert "-DCMAKE_CXX_COMPILER=clang++-18" in argv


def test_a_campaign_missing_its_toolchain_pins_is_refused() -> None:
    """Each of these silently produces an empty or meaningless corpus."""

    subject = campaign()
    subject.options = dict(subject.options, llvm_version=None)
    with pytest.raises(CampaignError, match="llvm_version"):
        _llvm_version(subject)
    assert _llvm_version(campaign()) == "18"

    subject = campaign()
    subject.options = {
        k: v for k, v in subject.options.items() if k != "cmake_source_dir"
    }
    with pytest.raises(CampaignError, match="cmake_source_dir"):
        _configure_argv(subject, WORKTREE, Path("/b"), "18")

    subject = campaign()
    subject.options = {k: v for k, v in subject.options.items() if k != "executables"}
    with pytest.raises(CampaignError, match="executables"):
        _executables(subject)
    subject.options = {"executables": "test_kernels"}
    with pytest.raises(CampaignError, match="executables"):
        _executables(subject)


def test_every_status_mull_can_report_is_mapped_and_none_is_guessed() -> None:
    """A mutant that never compiled or was skipped is not a kill.

    Only KILLED and SURVIVED reach the denominator, so folding CompileError or
    Ignored into either would credit the suite with detections it never made.
    """

    rows = _rows(
        report(
            mutant(status="Killed", line=2, column=12),
            mutant(status="Survived", line=2, column=20),
            mutant(status="NoCoverage", line=3, column=10),
            mutant(status="Timeout", line=3, column=12),
            mutant(status="CompileError", line=4, column=3),
            mutant(status="Ignored", line=4, column=4),
            mutant(status="RuntimeError", line=5, column=1),
        ),
        WORKTREE,
    )
    assert [row["outcome"] for row in rows] == [
        "KILLED",
        "SURVIVED",
        "NO_COVERAGE",
        "TIMED_OUT",
        "UNVIABLE",
        "UNVIABLE",
        "ERROR",
    ]

    with pytest.raises(CampaignError, match="unknown status"):
        _rows(report(mutant(status="Flaky")), WORKTREE)


def test_the_original_text_is_sliced_out_of_the_source_the_report_carries() -> None:
    """Mull publishes the replacement but not what it replaced.

    The original is what makes the mutant identity readable and stable, so it
    is read from the embedded source rather than guessed from the mutator name.
    """

    assert (
        _slice(
            SOURCE_TEXT,
            {"start": {"line": 2, "column": 21}, "end": {"line": 2, "column": 22}},
        )
        == "<"
    )
    assert (
        _slice(
            SOURCE_TEXT,
            {"start": {"line": 3, "column": 9}, "end": {"line": 3, "column": 10}},
        )
        == "*"
    )
    # A span crossing lines is joined, not truncated to its first line.
    multi = _slice(
        SOURCE_TEXT,
        {"start": {"line": 2, "column": 3}, "end": {"line": 4, "column": 4}},
    )
    assert multi.startswith("for (int i") and multi.endswith("}")
    assert "g(i * 2);" in multi
    # A location past the end of the file is empty, not an IndexError.
    assert (
        _slice(
            SOURCE_TEXT,
            {"start": {"line": 99, "column": 1}, "end": {"line": 99, "column": 2}},
        )
        == ""
    )


def test_a_mutant_is_named_independently_of_the_line_it_sits_on() -> None:
    """Naming by line would churn the baseline whenever the code above moves."""

    early = _rows(report(mutant(line=2, column=21, end_column=22)), WORKTREE)
    moved = "\n\n\n" + SOURCE_TEXT
    late = _rows(
        report(mutant(line=5, column=21, end_column=22), source=moved), WORKTREE
    )
    assert early[0]["id"] == late[0]["id"]
    assert early[0]["line"] == 2 and late[0]["line"] == 5


def test_repeats_of_one_mutation_in_a_file_are_separate_mutants() -> None:
    """Two identical `<` -> `<=` edits are two mutants, in file order."""

    rows = _rows(
        report(
            mutant(line=3, column=21, end_column=22),
            mutant(line=2, column=21, end_column=22),
        ),
        WORKTREE,
    )
    assert [row["line"] for row in rows] == [2, 3]
    assert len({row["id"] for row in rows}) == 2


def test_every_receipt_field_a_row_carries_is_pinned() -> None:
    """The row keys are the receipt's schema, and nothing else checks them."""

    (row,) = _rows(report(mutant(line=2, column=21, end_column=22)), WORKTREE)
    assert set(row) == {
        "id",
        "outcome",
        "path",
        "line",
        "operator",
        "original_text",
        "mutated_text",
    }
    assert row["path"] == SOURCE
    assert row["line"] == 2
    assert row["operator"] == "cxx_lt_to_le"
    assert row["original_text"] == "<"
    assert row["mutated_text"] == "<="


def test_a_mutant_outside_the_worktree_stops_the_run() -> None:
    """Mull reports absolute paths, including ones from system headers.

    Such a mutant cannot be checked against the campaign's source hashes, so a
    corpus containing one is refused rather than scored with a path nothing in
    the manifest pins.
    """

    outside = {
        "files": {
            "/usr/include/c++/13/cmath": {"source": SOURCE_TEXT, "mutants": [mutant()]}
        }
    }
    with pytest.raises(CampaignError, match="outside the worktree"):
        _rows(outside, WORKTREE)


def test_a_kill_by_either_suite_outranks_a_survival_in_the_other() -> None:
    """Both binaries link the same kernels, so one mutant appears in both.

    Taking the last report's status instead would let the order the executables
    happen to run in decide the score.
    """

    killed = report(mutant(status="Killed", identifier="m1"))
    survived = report(mutant(status="Survived", identifier="m1"))
    for pair in ((killed, survived), (survived, killed)):
        (row,) = _rows(_merge(pair), WORKTREE)
        assert row["outcome"] == "KILLED"

    # A mutant one suite never reaches is not evidence against one that did.
    uncovered = report(mutant(status="NoCoverage", identifier="m1"))
    (row,) = _rows(_merge((uncovered, survived)), WORKTREE)
    assert row["outcome"] == "SURVIVED"


def test_the_manifests_source_globs_scope_a_corpus_the_tool_cannot_scope() -> None:
    """Mull mutates every translation unit in the binary, tests included.

    50 of the 596 mutants this suite produced live in test_kernels.cpp itself.
    Scoring a campaign about the kernels on mutations of its own test file
    would measure something nobody asked about.
    """

    scope = campaign().source
    assert scope == ("aria_core/src/cpu/**",)
    assert _in_scope("aria_core/src/cpu/math_space.cpp", scope)
    assert _in_scope("aria_core/src/cpu/simd_elementwise.h", scope)
    assert not _in_scope("aria_designer/runtime/tests/test_kernels.cpp", scope)


def test_a_recursive_glob_reaches_a_subdirectory_that_does_not_exist_yet() -> None:
    """`PurePath.match` treats a trailing `**` as one `*` before Python 3.13.

    `aria_core/src/cpu` is flat today, so the difference is invisible: the
    campaign would keep passing while every mutant in a newly added
    subdirectory fell silently out of the corpus. A scope that shrinks without
    saying so is the failure this whole exercise exists to catch.
    """

    scope = ("aria_core/src/cpu/**",)
    assert _in_scope("aria_core/src/cpu/simd/avx512.cpp", scope)
    assert _in_scope("aria_core/src/cpu/a/b/c/deep.cpp", scope)
    assert not _in_scope("aria_core/src/gpu/kernel.cu", scope)
    # A single star still stops at the separator, or every narrow glob in every
    # manifest would quietly widen to the whole tree below it.
    assert _in_scope("aria_core/src/cpu/x.cpp", ("aria_core/src/cpu/*.cpp",))
    assert not _in_scope("aria_core/src/cpu/sub/x.cpp", ("aria_core/src/cpu/*.cpp",))


def test_the_campaign_under_test_is_wired_end_to_end() -> None:
    """The manifest this adapter was proven on is loadable and pins real files."""

    loaded = campaign()
    assert loaded.mutation_engine == "mull"
    assert loaded.language == "cpp"
    assert loaded.jobs == 1, "a host-sized worker count makes the survivor set jitter"
    assert loaded.survivor_baseline, "the recorded survivor baseline must not be empty"
    assert _executables(loaded) == ("test_kernels", "test_runtime")
    for relative in loaded.source_sha256:
        assert (REPO_ROOT / relative).is_file(), relative


def test_the_recorded_baseline_is_the_corrected_measurement_not_the_broken_one() -> (
    None
):
    """The first run of this suite reported 4616 survivors over 4926 mutants.

    That run had no coverage data. Recording its survivor set would have frozen
    a baseline of unreachable code that no test could ever close, and every
    later run would have read as RATCHET_HELD forever.
    """

    manifest = json.loads((REPO_ROOT / MANIFEST).read_text(encoding="utf-8"))
    assert len(manifest["survivor_baseline"]) == 202
    assert "--coverage-info" in _engine_argv(
        campaign(),
        "/bin/mull-runner-18",
        Path("/b/t"),
        Path("/b/t.profdata"),
        Path("/b/r"),
        "t",
    )


def test_the_committed_campaign_makes_uninitialised_reads_deterministic() -> None:
    """Two identical jobs=1 runs disagreed on three mutants until this was pinned.

    `cxx_mul_to_div` at linalg.cpp:302 shortens the memset that zeroes the matmul
    output buffer, and `cxx_le_to_gt` at math_space.cpp:54 empties the inner
    triangular loop so dist/weights are never written. Both mutants are caught
    only when the allocator happens to return a dirty page, so their verdicts
    flipped between runs. A survivor set that depends on the allocator cannot
    carry a ratchet, and a kill that depends on it was never a test of anything.
    """

    assert campaign().environment.get("MALLOC_PERTURB_"), (
        "an uninitialised read must fail the same way every run"
    )
