"""Generated mutants for the C and C++ lanes, via Mull.

Mull mutates at the LLVM IR level: a clang pass plugin embeds every candidate
mutation into the object code behind a runtime switch, so one build carries the
whole corpus and `mull-runner` re-runs the same binary with one mutation enabled
at a time. That is why this adapter builds the tree itself instead of taking a
prebuilt binary -- the instrumented build *is* the mutant corpus.

Three of Mull's behaviours would silently corrupt a verdict:

* **Without coverage data, Mull reports almost everything as survived.** Handed
  no `--coverage-info`, the runner has no way to know which mutants any test
  reaches; the first run of this adapter against `aria_kernels` returned
  ``{Survived: 4616, Timeout: 309, Killed: 1}`` over a suite that does in fact
  detect a broken kernel -- most of those mutants live in translation units the
  22 test functions never call. So the profdata is mandatory, `execute` refuses
  to run without it, and `--include-not-covered` is never passed.
* **`--timeout` is milliseconds.** The manifest field is seconds, like every
  other engine here, so the conversion happens in one place.
* **The effective timeout is `max(baseline * 10, minimum-timeout)`.** These
  suites run in under 10ms, so `baseline * 10` rounds to zero and every mutant
  that does any work at all is recorded as a timeout -- 309 of them on the first
  run. The floor is therefore pinned from the manifest, never defaulted.

Exit code is not the verdict here either: `mull-runner` exits non-zero whenever
a mutant survived, which for an uncurated corpus is the ordinary case. The
survivor SET in the Mutation Testing Elements report decides, scored by the
shared core.
"""

from __future__ import annotations

import json
import os
import re
import shutil
from functools import lru_cache
from collections.abc import Mapping, Sequence
from pathlib import Path, PurePath
from typing import Any

from conductor import mutation_engine_generated as _core
from conductor.mutation_scope import CampaignError

ENGINE = "mull"

# Mull's Elements vocabulary, read out of the runner's own string table, mapped
# onto the receipt's. `Ignored` is a mutant the tool was told to skip: like one
# that did not compile it was never a test of anything, so it is recorded and
# left out of the score rather than counted as a kill.
_OUTCOMES = {
    "Killed": _core.KILLED,
    "Survived": _core.SURVIVED,
    "NoCoverage": _core.NO_COVERAGE,
    "Timeout": _core.TIMED_OUT,
    "CompileError": _core.UNVIABLE,
    "Ignored": _core.UNVIABLE,
    "RuntimeError": _core.ERROR,
}

# The instrumentation the corpus needs, and the instrumentation the coverage
# filter needs. Both go into the same build: Mull reads the profdata produced by
# the coverage-mapped binary to decide which mutants any test actually reaches.
MUTATION_FLAGS = ("-g", "-grecord-command-line")
COVERAGE_FLAGS = ("-fprofile-instr-generate", "-fcoverage-mapping")


def _llvm_version(campaign: _core.GeneratedCampaign) -> str:
    """The LLVM release the toolchain is pinned to.

    Mull's pass plugin is compiled against one LLVM version and silently
    produces nothing useful under another, so the version is manifest data
    rather than whatever `clang++` happens to resolve to.
    """

    version = campaign.options.get("llvm_version")
    if not version:
        raise CampaignError(
            "generator.options.llvm_version must name the LLVM release the Mull "
            "pass plugin was built against (e.g. 18)"
        )
    return str(version)


def _tool(name: str, version: str, hint: str) -> str:
    """A version-suffixed LLVM-family executable, or a refusal naming it."""

    found = shutil.which(f"{name}-{version}")
    if found is None:
        raise CampaignError(f"{name}-{version} is not installed; {hint}")
    return found


def binary() -> str:
    """The newest mull-runner on PATH.

    The core resolves the engine binary before it has a campaign in hand, so
    the manifest's LLVM version cannot be consulted here. `execute` re-resolves
    against the manifest and refuses a mismatch rather than quietly running the
    wrong runner against a plugin built for another LLVM.
    """

    found = sorted(
        (
            (int(match.group(1)), str(path))
            for directory in os.environ.get("PATH", "").split(os.pathsep)
            if directory
            for path in Path(directory).glob("mull-runner-*")
            if (match := re.fullmatch(r"mull-runner-(\d+)", path.name))
            and os.access(path, os.X_OK)
        ),
        reverse=True,
    )
    if not found:
        raise CampaignError(
            "no mull-runner-<llvm> is installed; install the Mull release whose "
            "asset name carries the same LLVM version as the clang it will "
            "build with (it is a developer tool, not a shipped dependency, so "
            "it belongs in the toolchain rather than in pyproject)"
        )
    return found[0][1]


def _plugin(version: str) -> str:
    """Where the clang pass plugin that embeds the mutants lives.

    Path construction only. Assembling the configure argv must not depend on
    what is installed on the host, or the resulting flags cannot be asserted
    anywhere the toolchain is absent -- which is every CI runner. The refusal
    lives in `_require_plugin`, which the build path calls before a build is
    paid for, so a missing plugin still fails at exactly the same moment.
    """

    return f"/usr/lib/mull-ir-frontend-{version}"


def _require_plugin(version: str) -> None:
    """Refuse a host with no pass plugin, before any build work starts.

    The path is not returned: `_configure_argv` calls `_plugin` itself, so a
    returned value would be discarded at the only call site, and a mutant that
    replaced it with `None` would be unkillable by any test.
    """

    path = _plugin(version)
    if not Path(path).is_file():
        raise CampaignError(
            f"the Mull pass plugin is not at {path}; without it the build "
            "carries no mutants and every campaign would report an empty corpus"
        )


def _executables(campaign: _core.GeneratedCampaign) -> tuple[str, ...]:
    """The test binaries to mutate, relative to the build directory."""

    named = campaign.options.get("executables") or ()
    if isinstance(named, str) or not isinstance(named, Sequence) or not named:
        raise CampaignError(
            "generator.options.executables must be a non-empty list of test "
            "binaries, relative to the build directory"
        )
    return tuple(str(name) for name in named)


def _configure_argv(
    campaign: _core.GeneratedCampaign, worktree: Path, build: Path, version: str
) -> list[str]:
    """The cmake configure step, with both instrumentations in the flags."""

    source = campaign.options.get("cmake_source_dir")
    if not source:
        raise CampaignError(
            "generator.options.cmake_source_dir must name the directory holding "
            "the CMakeLists.txt for the suite under test"
        )
    flags = " ".join(
        (f"-fpass-plugin={_plugin(version)}", *MUTATION_FLAGS, *COVERAGE_FLAGS)
    )
    argv = [
        "cmake",
        "-S",
        str(worktree / str(source)),
        "-B",
        str(build),
        "-G",
        "Ninja",
        f"-DCMAKE_C_COMPILER=clang-{version}",
        f"-DCMAKE_CXX_COMPILER=clang++-{version}",
        f"-DCMAKE_C_FLAGS={flags}",
        f"-DCMAKE_CXX_FLAGS={flags}",
        # The profile runtime has to be linked in, or the binary runs fine and
        # writes no .profraw, and the coverage filter silently sees nothing.
        # detect-secrets reads the flag as a base64 high-entropy string; it is a
        # clang linker flag, and the scan only sees it because this file changed.
        "-DCMAKE_EXE_LINKER_FLAGS=-fprofile-instr-generate",  # pragma: allowlist secret
    ]
    for define in campaign.options.get("cmake_args", ()):
        argv.append(str(define))
    return argv


def _engine_argv(
    campaign: _core.GeneratedCampaign,
    binary_path: str,
    executable: Path,
    profdata: Path,
    report_dir: Path,
    report_name: str,
) -> list[str]:
    """The mull-runner invocation, every bound pinned by the manifest."""

    milliseconds = campaign.mutant_timeout_seconds * 1000
    argv = [
        binary_path,
        str(executable),
        # Without this the runner cannot tell reached code from unreached and
        # reports the whole binary as survived. It is the difference between a
        # measurement and a number.
        "--coverage-info",
        str(profdata),
        "--workers",
        str(campaign.jobs),
        "--timeout",
        str(milliseconds),
        # Floor for max(baseline * 10, minimum-timeout); these suites finish in
        # microseconds, so without it the computed timeout is zero.
        "--minimum-timeout",
        str(milliseconds),
        "--reporters",
        "Elements",
        "--report-dir",
        str(report_dir),
        "--report-name",
        report_name,
    ]
    for mutator in campaign.exclude:
        argv += ["--ignore-mutators", mutator]
    return argv


def _slice(source: str, location: Mapping[str, Any]) -> str:
    """The text a mutant replaced, read out of the source the report carries.

    Mull publishes the replacement but not the original. The Elements report
    embeds each file's source, so the span identifies the original exactly --
    and unlike the line number, the text does not move when the code above it
    is edited.
    """

    lines = source.splitlines()
    start, end = location["start"], location["end"]
    first, last = int(start["line"]), int(end["line"])
    if not 1 <= first <= len(lines):
        return ""
    if first == last:
        return lines[first - 1][int(start["column"]) - 1 : int(end["column"]) - 1]
    body = [lines[first - 1][int(start["column"]) - 1 :]]
    body += lines[first : last - 1]
    if last <= len(lines):
        body.append(lines[last - 1][: int(end["column"]) - 1])
    return "\n".join(body)


_GLOB_TOKEN = re.compile(r"\*\*|[*?]|[^*?]+")
_GLOB_CLASS = {"**": ".*", "*": "[^/]*", "?": "[^/]"}


@lru_cache(maxsize=None)
def _matcher(pattern: str) -> re.Pattern[str]:
    """A glob compiled with `**` crossing directory separators and `*` not.

    `PurePath.match` is not usable here: before Python 3.13 it treats a trailing
    `**` as a single `*`, so `aria_core/src/cpu/**` matches a file in that
    directory and silently misses one in a subdirectory of it. A campaign whose
    scope quietly shrank when someone added a subdirectory would keep reporting
    green over a corpus it had stopped measuring.

    The scan is a `findall` rather than an index walked by hand so that no
    single-token edit can stop it advancing: five mutants of the arithmetic this
    replaces ran forever and scored the campaign ERROR instead of failing it.
    `_GLOB_TOKEN` alternates longest-first, so `**` wins over `*` and a run of
    literal characters is escaped in one piece — `re.escape` over a run is the
    concatenation of the per-character escapes, so the emitted pattern is
    identical to the one the hand-walked index produced.
    """

    out = [
        _GLOB_CLASS.get(token) or re.escape(token)
        for token in _GLOB_TOKEN.findall(pattern)
    ]
    return re.compile(f"^{''.join(out)}$")


def _in_scope(relative: str, patterns: Sequence[str]) -> bool:
    """Whether a mutated file is one the campaign declared.

    Unlike fest and cargo-mutants, Mull takes no source filter: it mutates
    every translation unit linked into the binary, which here includes the test
    file itself and any library the suite happens to pull in. The manifest's
    `source` globs are therefore applied to the report rather than to the tool,
    so a campaign scoped to the kernels is not silently scored on its own tests.
    """

    posix = PurePath(relative).as_posix()
    return any(_matcher(pattern).match(posix) for pattern in patterns)


def _rows(report: Mapping[str, Any], worktree: Path) -> list[dict[str, Any]]:
    """Name and record every mutant the run produced, deterministically."""

    found = []
    for path, entry in sorted(report.get("files", {}).items()):
        try:
            relative = str(Path(path).resolve().relative_to(worktree.resolve()))
        except ValueError:
            # Mull reports absolute paths; a mutant outside the tree under test
            # cannot be named against the campaign's source hashes.
            raise CampaignError(
                f"Mull reported a mutant outside the worktree: {path}"
            ) from None
        source = str(entry.get("source", ""))
        for mutant in entry.get("mutants", ()):
            found.append((relative, source, mutant))
    found.sort(
        key=lambda item: (
            item[0],
            int(item[2]["location"]["start"]["line"]),
            int(item[2]["location"]["start"]["column"]),
            str(item[2]["mutatorName"]),
        )
    )

    entries = [
        (
            relative,
            str(mutant["mutatorName"]),
            _slice(source, mutant["location"]),
            str(mutant["replacement"]),
        )
        for relative, source, mutant in found
    ]
    names = _core.identify(entries)

    rows: list[dict[str, Any]] = []
    for (_, _, mutant), identifier, entry in zip(found, names, entries, strict=True):
        status = str(mutant.get("status", ""))
        if status not in _OUTCOMES:
            raise CampaignError(f"Mull reported unknown status {status!r}")
        rows.append(
            {
                "id": identifier,
                "outcome": _OUTCOMES[status],
                "path": entry[0],
                "line": int(mutant["location"]["start"]["line"]),
                "operator": entry[1],
                "original_text": entry[2],
                "mutated_text": entry[3],
            }
        )
    return rows


# Most to least informative: a kill by any suite outranks a survival, and a
# mutant some suite reached outranks one nothing covered.
_RANK = ("Killed", "Timeout", "Survived", "CompileError", "Ignored", "NoCoverage")


def _better(candidate: str, incumbent: str) -> bool:
    order = {status: index for index, status in enumerate(_RANK)}
    return order.get(candidate, len(_RANK)) < order.get(incumbent, len(_RANK))


def _merge(reports: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    """One report per executable, folded into one corpus.

    A mutant reached by two suites is one mutant, and the better of its two
    outcomes is the true one: if either suite killed it, it is killed. Taking
    the last-written status instead would let the order the binaries happen to
    run in decide the score.
    """

    merged: dict[str, dict[str, Any]] = {}
    for report in reports:
        for path, entry in report.get("files", {}).items():
            into = merged.setdefault(
                path, {"source": entry.get("source", ""), "mutants": {}}
            )
            for mutant in entry.get("mutants", ()):
                key = str(mutant["id"])
                seen = into["mutants"].get(key)
                if seen is None or _better(str(mutant["status"]), str(seen["status"])):
                    into["mutants"][key] = mutant
    return {
        "files": {
            path: {
                "source": entry["source"],
                "mutants": list(entry["mutants"].values()),
            }
            for path, entry in merged.items()
        }
    }


def _profile(
    executable: Path,
    build: Path,
    name: str,
    *,
    campaign: _core.GeneratedCampaign,
    receipt: dict[str, Any],
    environment: Mapping[str, str],
    profdata_tool: str,
) -> Path:
    """Run one suite green, and merge its raw profile into coverage data."""

    raw = build / f"{name}.profraw"
    raw.unlink(missing_ok=True)
    result, _ = _core.run(
        [str(executable)],
        cwd=build,
        timeout_seconds=campaign.run_timeout_seconds,
        environment={**environment, "LLVM_PROFILE_FILE": str(raw)},
    )
    receipt.setdefault("baselines", {})[name] = result.as_dict()
    if result.timed_out or result.returncode != 0:
        raise CampaignError(
            f"unmutated baseline {name} failed (rc={result.returncode}); every "
            "mutant after an already-red suite reads as caught"
        )
    if not raw.is_file():
        raise CampaignError(
            f"{name} produced no profile at {raw}: without coverage data Mull "
            "reports unreached code as survived"
        )

    profdata = build / f"{name}.profdata"
    merged, _ = _core.run(
        [profdata_tool, "merge", "-sparse", str(raw), "-o", str(profdata)],
        cwd=build,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
    )
    if merged.returncode != 0 or not profdata.is_file():
        raise CampaignError(
            f"llvm-profdata could not merge {raw}: {merged.stderr_tail}"
        )
    return profdata


def _build(
    campaign: _core.GeneratedCampaign,
    worktree: Path,
    build: Path,
    version: str,
    *,
    receipt: dict[str, Any],
    output_path: Path,
    environment: dict[str, str],
) -> None:
    """Configure and build the instrumented corpus, or refuse with the receipt written."""

    _require_plugin(version)  # before the first build command, as it always was
    for argv in (
        _configure_argv(campaign, worktree, build, version),
        ["cmake", "--build", str(build)],
    ):
        result, _ = _core.run(
            argv,
            cwd=worktree,
            timeout_seconds=campaign.run_timeout_seconds,
            environment=environment,
        )
        receipt.setdefault("build", []).append(result.as_dict())
        if result.timed_out or result.returncode != 0:
            receipt["status"] = "BASELINE_FAILED"
            _core.atomic_json(output_path, receipt)
            raise CampaignError(
                f"the instrumented build failed: {result.stderr_tail[-800:]}"
            )


def _engine_reports(
    campaign: _core.GeneratedCampaign,
    binary: str,
    build: Path,
    *,
    receipt: dict[str, Any],
    environment: dict[str, str],
    profdata_tool: str,
) -> list[dict[str, Any]]:
    """Cover then mutate every declared executable; one Elements report each."""

    reports = []
    for name in _executables(campaign):
        executable = build / name
        if not executable.is_file():
            raise CampaignError(f"the build produced no executable at {executable}")
        profdata = _profile(
            executable,
            build,
            Path(name).name,
            campaign=campaign,
            receipt=receipt,
            environment=environment,
            profdata_tool=profdata_tool,
        )
        report_dir = build / "mull-reports"
        result, _ = _core.run(
            _engine_argv(
                campaign, binary, executable, profdata, report_dir, Path(name).name
            ),
            cwd=build,
            timeout_seconds=campaign.run_timeout_seconds,
            environment=environment,
        )
        receipt.setdefault("engine_result", []).append(result.as_dict())
        if result.timed_out:
            raise CampaignError(
                f"mull-runner timed out on {name} after {campaign.run_timeout_seconds}s"
            )
        # A non-zero exit means "mutants survived", the ordinary reading of an
        # uncurated corpus. The report decides, not the exit code.
        path = report_dir / f"{Path(name).name}.json"
        if not path.is_file():
            raise CampaignError(
                f"mull-runner wrote no Elements report at {path} "
                f"(rc={result.returncode}): {result.stderr_tail[-800:]}"
            )
        try:
            reports.append(json.loads(path.read_text(encoding="utf-8")))
        except json.JSONDecodeError as exc:
            raise CampaignError(f"Mull's report at {path} is not JSON: {exc}") from exc
    return reports


def execute(
    campaign: _core.GeneratedCampaign,
    receipt: dict[str, Any],
    *,
    binary: str,
    worktree: Path,
    output_path: Path,
) -> None:
    """Build the instrumented corpus, cover it, mutate it, fill the receipt."""

    if drifted := _core.drift(campaign, worktree):
        raise CampaignError(f"snapshot source hashes drifted: {drifted}")
    version = _llvm_version(campaign)
    wanted = _tool(
        "mull-runner",
        version,
        f"the manifest pins llvm_version={version}",
    )
    if Path(binary).resolve() != Path(wanted).resolve():
        raise CampaignError(
            f"the resolved runner {binary} is not the manifest's {wanted}: a "
            f"runner built for another LLVM release reads the pass plugin's "
            f"output as an unmutated binary"
        )
    profdata_tool = _tool(
        "llvm-profdata", version, f"install llvm-{version} beside clang-{version}"
    )
    from conductor.mutation_run_scope import mull_scope_config

    environment = {
        **campaign.environment,
        "PATH": os.environ.get("PATH", ""),
        "MULL_CONFIG": str(mull_scope_config(campaign, worktree)),
    }

    build = worktree / str(campaign.options.get("build_dir", ".mull-build"))
    _build(
        campaign,
        worktree,
        build,
        version,
        receipt=receipt,
        output_path=output_path,
        environment=environment,
    )

    # The declared suite gate. The per-executable runs below exist to produce
    # coverage profiles, one file each; this is the campaign's own statement of
    # what "the tests pass" means, and it runs before any mutation work is paid
    # for. Every mutant after an already-red suite would read as caught.
    baseline_argv = list(campaign.test_argv)
    baseline, _ = _core.run(
        baseline_argv,
        cwd=build,
        timeout_seconds=campaign.run_timeout_seconds,
        environment=environment,
    )
    _core.note_baseline(campaign, receipt, baseline, baseline_argv, output_path)

    reports = _engine_reports(
        campaign,
        binary,
        build,
        receipt=receipt,
        environment=environment,
        profdata_tool=profdata_tool,
    )

    receipt["engine_version"] = reports[0].get("config", {}).get("mullVersion")
    every = _rows(_merge(reports), worktree)
    receipt["mutants"] = [
        row for row in every if _in_scope(row["path"], campaign.source)
    ]
    if not receipt["mutants"]:
        raise CampaignError(
            f"none of the {len(every)} mutants Mull produced fall under "
            f"generator.source {list(campaign.source)}; the campaign would "
            "score an empty corpus"
        )
    receipt["engine_summary"] = {
        "mutants_reported": len(every),
        "mutants_in_scope": len(receipt["mutants"]),
        "files_mutated": sorted({row["path"] for row in every}),
    }
    tested = sum(
        1
        for row in receipt["mutants"]
        if row["outcome"] in (_core.KILLED, _core.SURVIVED)
    )
    _core.require_executed(len(receipt["mutants"]), tested, campaign.source)


def main(argv: list[str] | None = None) -> int:
    """Run a Mull campaign; the shared CLI dispatches by engine."""

    return _core.main(argv)


if __name__ == "__main__":
    raise SystemExit(main())
