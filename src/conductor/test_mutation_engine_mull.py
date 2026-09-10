"""Contracts for the Mull adapter -- the C and C++ half.

Everything engine-agnostic (naming, scoring, refusals) is proven in
`test_mutation_engine_generated`. What is left here is the handful of Mull
behaviours that turn a run into a number that looks fine and means nothing:
the millisecond timeout, the timeout floor, and above all the coverage filter
without which the runner reports unreached code as survived.
"""

from __future__ import annotations

import os
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
from conductor import mutation_engine_mull as mull
from conductor.mutation_engine_generated import CommandResult, load_generated_campaign
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
    subject.options["cmake_args"] = ["-DENABLE_TESTS=ON", "-DBUILD_SHARED_LIBS=OFF"]
    argv = _configure_argv(subject, WORKTREE, Path("/b"), "18")
    assert argv[:7] == [
        "cmake",
        "-S",
        str(WORKTREE / subject.options["cmake_source_dir"]),
        "-B",
        "/b",
        "-G",
        "Ninja",
    ]
    assert argv[-2:] == ["-DENABLE_TESTS=ON", "-DBUILD_SHARED_LIBS=OFF"]
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
    assert (
        _slice(
            "abcd\nefgh\nijkl",
            {"start": {"line": 1, "column": 2}, "end": {"line": 3, "column": 3}},
        )
        == "bcd\nefgh\nij"
    )
    # A location past the end of the file is empty, not an IndexError.
    assert (
        _slice(
            SOURCE_TEXT,
            {"start": {"line": 99, "column": 1}, "end": {"line": 99, "column": 2}},
        )
        == ""
    )
    # Missing embedded source must not invent text for a stable mutant identity.
    missing_source = report(mutant(line=1, column=1, end_column=2))
    entry = missing_source["files"][str(WORKTREE / SOURCE)]
    del entry["source"]
    assert _rows(missing_source, WORKTREE)[0]["original_text"] == ""
    merged = _merge([missing_source])
    assert merged["files"][str(WORKTREE / SOURCE)]["source"] == ""
    assert (
        _merge([report(mutant())])["files"][str(WORKTREE / SOURCE)]["source"]
        == SOURCE_TEXT
    )
    del entry["mutants"][0]["status"]
    with pytest.raises(CampaignError, match="unknown status ''"):
        _rows(missing_source, WORKTREE)
    assert (
        _slice(
            "abc\ndef",
            {"start": {"line": 0, "column": 1}, "end": {"line": 0, "column": 2}},
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


# --------------------------------------------------------------- orchestration
#
# `_build` and `_engine_reports` were the two halves of `execute`, and nothing
# had ever run either of them: every test above stops at the argv. A build that
# reports success it never earned, or a corpus quietly measured over one
# executable instead of two, is exactly the failure this adapter exists to make
# impossible -- so the orchestration is driven here against a faked `_core.run`.


def _result(returncode: int = 0, *, stderr: str = "") -> CommandResult:
    """One finished command, in the shape `_core.run` hands its caller."""

    return CommandResult(
        returncode=returncode,
        timed_out=False,
        duration_seconds=0.0,
        stdout_tail="",
        stderr_tail=stderr,
    )


def _recorded_runs(
    monkeypatch: pytest.MonkeyPatch, *outcomes: CommandResult
) -> list[list[str]]:
    """Replace `_core.run` with a recorder, returning the argv list it fills."""

    calls: list[list[str]] = []

    def fake_run(argv, *, cwd, timeout_seconds, environment):
        calls.append([str(arg) for arg in argv])
        index = len(calls) - 1
        return (outcomes[index] if index < len(outcomes) else _result()), ""

    monkeypatch.setattr(mull._core, "run", fake_run)
    return calls


def _installed_plugin(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Pretend the host carries the pass plugin, so `_build` gets past its gate."""

    plugin = tmp_path / "mull-ir-frontend-18"
    plugin.write_text("", encoding="utf-8")
    monkeypatch.setattr(mull, "_plugin", lambda version: str(plugin))


def test_a_host_with_no_pass_plugin_is_refused_before_a_configure_is_paid_for(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A cmake configure of the kernel suite is minutes of machine time.

    The pass plugin is what puts the mutants into the object files, so a build
    without it produces a clean binary and the campaign reports an empty corpus
    as a pass. The refusal has to come first, not after the build is paid for.
    """

    calls = _recorded_runs(monkeypatch)
    receipt: dict[str, object] = {}
    output = tmp_path / "receipt.json"
    with pytest.raises(CampaignError, match="mull-ir-frontend-99"):
        mull._build(
            campaign(),
            tmp_path,
            tmp_path / "build",
            "99",
            receipt=receipt,
            output_path=output,
            environment={},
        )
    assert calls == []
    assert receipt == {}
    assert not output.exists()


def test_a_failed_configure_lands_in_the_receipt_and_never_reaches_the_build(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A configure that fails must stop the run and say so on disk.

    Running `cmake --build` over a failed configure builds the previous tree, so
    the corpus would be measured against stale objects; and a receipt left
    unwritten is a campaign that failed silently.
    """

    _installed_plugin(monkeypatch, tmp_path)
    calls = _recorded_runs(monkeypatch, _result(1, stderr="ninja: not found"))
    receipt: dict[str, object] = {}
    output = tmp_path / "receipt.json"
    with pytest.raises(CampaignError, match="the instrumented build failed"):
        mull._build(
            campaign(),
            tmp_path,
            tmp_path / "build",
            "18",
            receipt=receipt,
            output_path=output,
            environment={},
        )
    assert len(calls) == 1
    assert "--build" not in calls[0]
    assert receipt["status"] == "BASELINE_FAILED"
    assert json.loads(output.read_text(encoding="utf-8"))["status"] == "BASELINE_FAILED"


def test_a_clean_configure_and_build_are_both_recorded_in_the_order_they_ran(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Both commands belong in the receipt, or the build is unauditable.

    The receipt is the only record that the corpus under measurement came from
    an instrumented configure at all; a build step that runs but is not recorded
    leaves a campaign nobody can reproduce.
    """

    _installed_plugin(monkeypatch, tmp_path)
    build = tmp_path / "build"
    calls = _recorded_runs(monkeypatch, _result(), _result())
    receipt: dict[str, object] = {}
    mull._build(
        campaign(),
        tmp_path,
        build,
        "18",
        receipt=receipt,
        output_path=tmp_path / "receipt.json",
        environment={},
    )
    assert calls[0][:1] == ["cmake"]
    assert calls[1] == ["cmake", "--build", str(build)]
    assert len(receipt["build"]) == 2
    assert "status" not in receipt


def _built_tree(tmp_path: Path, *, reports: bool) -> tuple[Path, tuple[str, ...]]:
    """A build directory holding every declared executable, reports optional."""

    build = tmp_path / "build"
    build.mkdir()
    names = _executables(campaign())
    for name in names:
        (build / name).write_text("", encoding="utf-8")
    if reports:
        report_dir = build / "mull-reports"
        report_dir.mkdir()
        for name in names:
            (report_dir / f"{Path(name).name}.json").write_text(
                json.dumps(report(mutant(status="Killed"))), encoding="utf-8"
            )
    return build, names


def test_every_declared_executable_is_covered_and_mutated_into_its_own_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Two declared executables must produce two profiles and two reports.

    Mull scores per binary. Stopping after the first one measures half the
    corpus and reports the number as if it covered all of it -- the campaign
    would go green while an entire suite went unmeasured.
    """

    build, names = _built_tree(tmp_path, reports=True)
    profiled: list[str] = []

    def fake_profile(executable, build_dir, name, **kwargs):
        profiled.append(name)
        return build_dir / f"{name}.profdata"

    monkeypatch.setattr(mull, "_profile", fake_profile)
    calls = _recorded_runs(monkeypatch, _result(), _result())
    receipt: dict[str, object] = {}
    reports = mull._engine_reports(
        campaign(),
        "/bin/mull-runner-18",
        build,
        receipt=receipt,
        environment={},
        profdata_tool="/bin/llvm-profdata-18",
    )
    assert profiled == [Path(name).name for name in names]
    assert len(calls) == len(names)
    assert len(reports) == len(names)
    assert len(receipt["engine_result"]) == len(names)


def test_a_missing_elements_report_stops_the_run_and_names_the_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """mull-runner exits non-zero whenever mutants survived, which is ordinary.

    The report is what decides the campaign, so its absence is the real failure
    and it cannot be inferred from the exit code. Reading past it would raise
    something about a missing file from deep inside the JSON parser instead.
    """

    build, _ = _built_tree(tmp_path, reports=False)
    monkeypatch.setattr(
        mull, "_profile", lambda executable, build_dir, name, **kwargs: build_dir
    )
    _recorded_runs(monkeypatch, _result(1))
    with pytest.raises(CampaignError, match="wrote no Elements report") as raised:
        mull._engine_reports(
            campaign(),
            "/bin/mull-runner-18",
            build,
            receipt={},
            environment={},
            profdata_tool="/bin/llvm-profdata-18",
        )
    assert "test_kernels.json" in str(raised.value)


# ------------------------------------------------------------------- execute
#
# `execute` is the only caller of everything above, and the four refusals it
# owns -- drifted sources, a runner from another LLVM release, a red baseline,
# and a report whose mutants all fall outside the declared scope -- are each the
# difference between a campaign that means something and one that reports a
# number over nothing. None of them had ever been run.


def _engine_report() -> dict[str, object]:
    """One Elements report carrying a killed, in-scope mutant and a version."""

    return {**report(mutant(status="Killed")), "config": {"mullVersion": "18.0.0"}}


def _drive_execute(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    *,
    binary: str = "/bin/mull-runner-18",
    drifted: dict[str, str] | None = None,
    baseline: int = 0,
    reports: list[dict[str, object]] | None = None,
    faked: dict[str, object] | None = None,
    subject: object | None = None,
) -> tuple[dict[str, object], list[list[str]]]:
    """Run `execute` with every subprocess and both halves of the build faked.

    `faked` collects what `execute` handed the two stubs. Without it the build
    directory and the child environment are computed and then dropped on the
    floor, so a mutant that empties either one changes nothing any test can see.
    """

    seen: dict[str, object] = {} if faked is None else faked

    from conductor import mutation_run_scope

    def _fake_scope(selected, worktree):
        seen["scope_source"] = selected.source
        seen["scope_worktree"] = worktree
        return tmp_path / "mull.yml"

    monkeypatch.setattr(mutation_run_scope, "mull_scope_config", _fake_scope)

    def _fake_build(campaign, worktree, build, version, **kwargs):
        seen["build"] = build
        seen["environment"] = kwargs.get("environment")

    def _fake_reports(*args, **kwargs):
        seen["report_args"] = [*args, *kwargs.values()]
        return [_engine_report()] if reports is None else reports

    monkeypatch.setattr(mull._core, "drift", lambda campaign, worktree: drifted or {})
    monkeypatch.setattr(
        mull, "_tool", lambda name, version, hint: f"/bin/{name}-{version}"
    )
    monkeypatch.setattr(mull, "_build", _fake_build)
    monkeypatch.setattr(mull, "_engine_reports", _fake_reports)
    calls = _recorded_runs(monkeypatch, _result(baseline))
    receipt: dict[str, object] = {}
    mull.execute(
        campaign() if subject is None else subject,
        receipt,
        binary=binary,
        worktree=REPO_ROOT,
        output_path=tmp_path / "receipt.json",
    )
    return receipt, calls


def test_a_run_over_drifted_sources_is_refused_before_anything_is_built(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The manifest pins the hashes of the sources it was reviewed against.

    Mull generates its own mutants, so a campaign over changed sources is not a
    re-run of the reviewed corpus -- it is a different corpus reported under the
    reviewed campaign's name.
    """

    with pytest.raises(CampaignError, match="snapshot source hashes drifted"):
        _drive_execute(
            monkeypatch, tmp_path, drifted={"aria_core/src/cpu/norm.cpp": "0"}
        )


def test_a_runner_from_another_llvm_release_is_refused_by_name(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The pass plugin and the runner have to come from the same release.

    A mismatched runner reads the plugin's output as an unmutated binary and
    reports every mutant as survived -- or, worse, as nothing at all.
    """

    with pytest.raises(CampaignError, match="is not the manifest's"):
        _drive_execute(monkeypatch, tmp_path, binary="/bin/mull-runner-17")


def test_a_red_baseline_stops_the_campaign_and_is_written_to_the_receipt(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Every mutant after an already-red suite reads as caught.

    The declared suite runs before any mutation work is paid for, and a failure
    has to reach disk: a campaign that stopped because its own baseline was
    broken must never be indistinguishable from one that never started.
    """

    output = tmp_path / "receipt.json"
    with pytest.raises(CampaignError, match="unmutated baseline failed"):
        _drive_execute(monkeypatch, tmp_path, baseline=1)
    assert json.loads(output.read_text(encoding="utf-8"))["status"] == "BASELINE_FAILED"


def test_a_clean_run_records_the_baseline_the_engine_version_and_the_scoped_corpus(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The receipt is the campaign's whole output; every field of it is load-bearing.

    `mutants_reported` against `mutants_in_scope` is what tells a reader that
    the scope filter dropped what it was meant to drop, rather than that Mull
    mutated less than anyone thought.
    """

    faked: dict[str, object] = {}
    receipt, calls = _drive_execute(monkeypatch, tmp_path, faked=faked)
    assert calls == [list(campaign().test_argv)]
    assert receipt["baseline_argv"] == list(campaign().test_argv)
    assert receipt["engine_version"] == "18.0.0"
    assert [row["outcome"] for row in receipt["mutants"]] == ["KILLED"]
    assert receipt["engine_summary"]["mutants_in_scope"] == 1
    assert receipt["engine_summary"]["mutants_reported"] == 1
    assert receipt["engine_summary"]["files_mutated"] == ["aria_core/src/cpu/norm.cpp"]
    assert "status" not in receipt
    # The build directory and the child environment never reach the receipt, so
    # the stubs are the only place their construction is observable.
    assert faked["build"] == REPO_ROOT / ".mull-build"
    assert faked["environment"]["PATH"] == os.environ["PATH"]
    assert faked["environment"]["MULL_CONFIG"] == str(tmp_path / "mull.yml")
    assert faked["scope_source"] == campaign().source
    assert faked["scope_worktree"] == REPO_ROOT
    assert "/bin/llvm-profdata-18" in faked["report_args"]


def test_the_build_directory_is_the_manifest_option_and_falls_back_to_mull_build(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The committed manifest sets `build_dir` to the same string as the default.

    So neither half of `options.get("build_dir", ".mull-build")` is observable
    from the committed campaign alone: drop the key and the default supplies the
    same path, change the default and the key overrides it. A campaign that
    declared a build directory outside the worktree would still have been built
    in `.mull-build`, and nothing would have said so.
    """

    declared = campaign()
    declared.options = dict(declared.options, build_dir="elsewhere")
    faked: dict[str, object] = {}
    _drive_execute(monkeypatch, tmp_path, faked=faked, subject=declared)
    assert faked["build"] == REPO_ROOT / "elsewhere"

    absent = campaign()
    absent.options = {k: v for k, v in absent.options.items() if k != "build_dir"}
    faked = {}
    _drive_execute(monkeypatch, tmp_path, faked=faked, subject=absent)
    assert faked["build"] == REPO_ROOT / ".mull-build"


def test_a_host_with_no_PATH_hands_the_build_an_empty_one_not_a_fabricated_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`os.environ.get("PATH", "")` has a fallback no ordinary run ever takes.

    Every host that runs this has a PATH, so the default is dead weight until
    the day it is not, and then it decides what the build can execute. Pinning
    it to the empty string keeps a PATH-less host failing at the first missing
    tool rather than searching somewhere nobody chose.
    """

    monkeypatch.delenv("PATH", raising=False)
    faked: dict[str, object] = {}
    _drive_execute(monkeypatch, tmp_path, faked=faked)
    assert faked["environment"]["PATH"] == ""


def test_a_corpus_that_all_falls_outside_the_declared_scope_is_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Mull mutates every translation unit in the binary, its own tests included.

    A report whose mutants are all out of scope means the campaign measured
    something nobody declared; scoring the empty remainder would report a
    perfect campaign over no mutants at all.
    """

    outside = {
        **report(mutant(status="Killed"), path="aria_designer/runtime/tests/t.cpp"),
        "config": {"mullVersion": "18.0.0"},
    }
    with pytest.raises(CampaignError, match="score an empty corpus"):
        _drive_execute(monkeypatch, tmp_path, reports=[outside])
