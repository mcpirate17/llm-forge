"""Shared core for conductor-managed campaigns whose mutants are engine-generated.

Only this automatic path may execute mutation testing. Legacy patch campaigns and
receipts are retained as archival provenance, never as an executable fallback.
A corpus that an agent selects measures the agent's selection, not the tests.

A generated engine takes the mutants from the source tree instead, so nobody can
curate them, and the number that matters stops being the score -- it is the
SURVIVOR SET. A campaign passes when no mutant survives that did not survive the
recorded baseline. New survivor, red campaign.

This module holds everything that does not depend on which engine ran: the
manifest model, mutant naming, the receipt envelope, the scoring rule and the
CLI. An engine adapter supplies two things -- where its binary is, and how to
turn one run into receipt rows. See `mutation_engine_fest` (Python) and
`mutation_engine_cargo` (Rust).
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import math
import os
import resource
import sys
from collections.abc import Mapping, Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Protocol

from conductor import mutation_testing_support as _support
from conductor.bytecode_isolation import isolated_python_env, scratch_root_for
from conductor.mutation_campaign_model import (
    RECEIPT_SCHEMA,
    REPO_ROOT,
    CommandResult,
    _runner_components_sha256,
    _sha256,
)
from conductor.mutation_scope import CampaignError
from conductor.project_paths import mutation_receipt_root_relative
from conductor.snapshot_worktree import isolated_snapshot, snapshot_python

# Re-exported so an adapter can write its own partial receipt without reaching
# into a private alias of a module it does not import.
atomic_json = _support.atomic_json


def write_receipt(path: Path, receipt: Mapping[str, Any]) -> None:
    """Write a receipt slim: summary plain, detail under one key.

    Every disk copy the engine leaves behind -- the RUNNING stub, the ERROR
    receipts, the final one -- goes through the same native codec, so a
    campaign with hundreds of mutants costs one small blob on disk instead of
    ~350 KB of per-mutant JSON. The in-memory dict stays full: attribution and
    the CLI summary read it before this is ever called.
    """

    from conductor.mutation_receipt_slim import write_slim_receipt

    write_slim_receipt(Path(path), receipt)


# engine name -> the module that adapts it. Imported lazily: each adapter
# imports this module, so naming them at import time would be circular.
ADAPTER_MODULES = {
    "fest": "conductor.mutation_engine_fest",
    "cargo-mutants": "conductor.mutation_engine_cargo",
    "mull": "conductor.mutation_engine_mull",
}
GENERATED_ENGINES = frozenset(ADAPTER_MODULES)
OUTPUT_TAIL_CHARS = 4000

# The receipt's outcome vocabulary. KILLED and SURVIVED are the only two that
# score; the rest are measurements the hand-written runner could not make.
# NO_COVERAGE is a mutant no test reaches, UNVIABLE one that does not compile,
# TIMED_OUT one the per-mutant bound cut off -- unknown, and deliberately
# counted as neither killed nor surviving so a slow kill is never misread as
# either.
KILLED = "KILLED"
SURVIVED = "SURVIVED"
TIMED_OUT = "TIMED_OUT"
NO_COVERAGE = "NO_COVERAGE"
UNVIABLE = "UNVIABLE"
ERROR = "ERROR"
OUTCOMES = (KILLED, SURVIVED, TIMED_OUT, NO_COVERAGE, UNVIABLE, ERROR)

# The per-mutant bound when the manifest does not pin one: three times the
# baseline suite's wall time, never less than a minute. Three because a mutant
# that hangs typically does so on the first test that touches it while an
# honest-but-slow kill pays the suite's full cost plus the mutant's own work;
# a floor because a suite that runs in two seconds says nothing about what a
# wedged mutant in it deserves.
MUTANT_TIMEOUT_BASELINE_MULTIPLIER = 3
MUTANT_TIMEOUT_FLOOR_SECONDS = 60


class EngineAdapter(Protocol):
    """What one generated engine has to provide."""

    ENGINE: str
    __file__: str

    @staticmethod
    def binary() -> str:
        """Path to the engine executable, or raise CampaignError."""

    @staticmethod
    def execute(
        campaign: GeneratedCampaign,
        receipt: dict[str, Any],
        *,
        binary: str,
        worktree: Path,
        output_path: Path,
    ) -> None:
        """Run the engine in the snapshot and fill `receipt["mutants"]`."""


class GeneratedCampaign:
    """A manifest for an engine that generates its own mutants.

    Deliberately not the frozen `Campaign` dataclass: that model requires a
    `mutations` array of reviewed patch files, and its loader is validated in
    Rust. A generated campaign has no patches to declare, so it declares the
    subject, the test command and the survivor set it is allowed to have.
    """

    __slots__ = (
        "manifest_path",
        "manifest_sha256",
        "campaign_id",
        "title",
        "language",
        "mutation_engine",
        "source",
        "exclude",
        "operators",
        "options",
        "seed",
        "jobs",
        "mutant_timeout_seconds",
        "run_timeout_seconds",
        "test_argv",
        "source_sha256",
        "test_sha256",
        "survivor_baseline",
        "survivor_baseline_recorded",
        "environment",
    )

    def __init__(self, manifest_path: Path, payload: Mapping[str, Any]) -> None:
        self.manifest_path = manifest_path
        self.manifest_sha256 = _sha256(manifest_path)
        self.campaign_id = _require(payload, "campaign_id", str)
        self.title = _require(payload, "title", str)
        self.language = _require(payload, "language", str)
        self.mutation_engine = _require(payload, "mutation_engine", str)
        if self.mutation_engine not in GENERATED_ENGINES:
            raise CampaignError(
                f"{self.mutation_engine!r} is not a generated engine; "
                f"known: {sorted(GENERATED_ENGINES)}"
            )
        generator = _require(payload, "generator", dict)
        self.source = tuple(_require(generator, "source", list))
        self.exclude = tuple(generator.get("exclude", ()))
        self.operators = tuple(generator.get("operators", ()))
        # Engine-specific knobs that mean nothing to the other engine, kept out
        # of the shared model so adding one never edits this class.
        self.options = dict(generator.get("options", {}))
        self.seed = int(generator.get("seed", 0))
        self.jobs = int(generator.get("jobs", 1))
        # None means the manifest pins no per-mutant bound and the run derives
        # one from the baseline suite's wall time once it exists (see
        # `resolve_mutant_timeout`). The adapters read this field only after
        # that resolution, so None never reaches an engine flag.
        self.mutant_timeout_seconds = generator.get("mutant_timeout_seconds")
        if self.mutant_timeout_seconds is not None:
            self.mutant_timeout_seconds = int(self.mutant_timeout_seconds)
        self.run_timeout_seconds = int(_require(generator, "run_timeout_seconds", int))
        self.test_argv = tuple(_require(payload, "test_argv", list))
        self.source_sha256 = dict(_require(payload, "source_sha256", dict))
        # The tests are pinned separately from the sources because they are not
        # mutated. Without this map a receipt would go on being accepted as
        # evidence for a test file that has since been rewritten, which is the
        # one thing a mutation receipt is supposed to make impossible.
        self.test_sha256 = dict(_require(payload, "test_sha256", dict))
        self.survivor_baseline = tuple(sorted(payload.get("survivor_baseline", ())))
        # A campaign that has never run has no survivor set to ratchet against,
        # so every survivor reads as new and it is red on its first run and on
        # every run after. The only green exit used to be hand-writing the
        # baseline, which KB-MUT-02 forbids. Generated manifests now declare
        # themselves unrecorded and the first run writes the list itself.
        # Manifests predating the flag are treated as recorded: their baselines
        # are real.
        # No flag and an empty baseline is indistinguishable from never having
        # run, and the two want the same treatment: record it. A non-empty
        # baseline was recorded by definition. Manifests written before the flag
        # existed are therefore handled without touching them.
        self.survivor_baseline_recorded = bool(
            payload.get(
                "survivor_baseline_recorded", bool(payload.get("survivor_baseline"))
            )
        )
        self.environment = dict(payload.get("environment", {}))
        if not self.source:
            raise CampaignError("generator.source must name at least one glob")
        if not self.source_sha256:
            raise CampaignError("source_sha256 must pin every mutated file")
        if not self.test_sha256:
            raise CampaignError("test_sha256 must pin every test file that ran")
        # `pytest a.py b.py` names its files; `ctest` and `cargo test` run a whole
        # suite and name none. Both are honest, and a suite runner covers more than
        # it declares, never less. What is not honest is naming some and quietly
        # claiming another, so the check bites exactly there.
        named = [path for path in self.test_sha256 if path in self.test_argv]
        if named and len(named) != len(self.test_sha256):
            missing = sorted(set(self.test_sha256) - set(named))
            raise CampaignError(
                f"test_argv names some declared tests but not {missing}; "
                "a campaign may only vouch for tests it actually executed"
            )


def _require(payload: Mapping[str, Any], key: str, kind: type) -> Any:
    if key not in payload:
        raise CampaignError(f"manifest is missing required key {key!r}")
    value = payload[key]
    if not isinstance(value, kind):
        raise CampaignError(
            f"{key!r} must be {kind.__name__}, got {type(value).__name__}"
        )
    return value


def load_generated_campaign(manifest_path: Path) -> GeneratedCampaign:
    """Load and validate one generated-engine manifest."""

    try:
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CampaignError(f"cannot read manifest {manifest_path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise CampaignError("manifest root must be a JSON object")
    return GeneratedCampaign(manifest_path, payload)


def manifest_engine(manifest_path: Path) -> str | None:
    """Peek at a manifest's declared engine without loading either model."""

    try:
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    engine = payload.get("mutation_engine") if isinstance(payload, dict) else None
    return engine if isinstance(engine, str) else None


def adapter_for(engine: str) -> EngineAdapter:
    """The adapter module for a declared engine."""

    if engine not in ADAPTER_MODULES:
        raise CampaignError(
            f"{engine!r} is not a generated engine; known: {sorted(GENERATED_ENGINES)}"
        )
    module = importlib.import_module(ADAPTER_MODULES[engine])
    return module  # type: ignore[return-value]


def mutant_id(
    relative_path: str,
    operator: str,
    original_text: str,
    mutated_text: str,
    occurrence: int,
) -> str:
    """A name for a generated mutant that survives an unrelated edit above it.

    Generated engines emit no stable ids, and the obvious substitutes are all
    unstable: line numbers move when anything above them changes, and byte
    offsets move when anything before them does. Naming a mutant by what it
    DOES -- operator, file, and the exact text rewrite -- keeps the survivor
    baseline meaningful across edits that did not touch the mutant. Identical
    rewrites in one file are disambiguated by their order in the file, which is
    the only part that can still churn.
    """

    digest = hashlib.sha256(
        "\x00".join((relative_path, operator, original_text, mutated_text)).encode(
            "utf-8"
        )
    ).hexdigest()[:12]
    return f"{operator}-{digest}-{occurrence}"


def identify(entries: Sequence[tuple[str, str, str, str]]) -> list[str]:
    """Name a run's mutants, in the order given, disambiguating repeats.

    `entries` are `(relative_path, operator, original_text, mutated_text)` in
    the deterministic order the adapter sorted them into -- file, then position.
    """

    seen: dict[str, int] = {}
    names: list[str] = []
    for relative, operator, original, mutated in entries:
        base = mutant_id(relative, operator, original, mutated, 0).rsplit("-", 1)[0]
        occurrence = seen.get(base, 0)
        seen[base] = occurrence + 1
        names.append(f"{base}-{occurrence}")
    return names


def drift(campaign: GeneratedCampaign, worktree: Path) -> dict[str, str]:
    """Files whose bytes in the snapshot are not the bytes the manifest pinned."""

    drifted: dict[str, str] = {}
    for relative, expected in campaign.source_sha256.items():
        target = worktree / relative
        if not target.is_file():
            drifted[relative] = "absent"
            continue
        actual = _sha256(target)
        if actual != expected:
            drifted[relative] = actual
    return drifted


def run(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: int,
    environment: Mapping[str, str],
    mutated_paths: Sequence[Path | str] = (),
) -> tuple[CommandResult, str]:
    """Run one bounded command, keeping full stdout for the caller to parse.

    Every command through here is a child of a mutation run -- the baseline
    suite, the engine binary (whose own per-mutant children inherit this
    environment), attribution's re-applied mutants -- and the engines' whole
    method is same-size, same-second rewrites of the files under test. The
    child environment is therefore always bound to a cache prefix private to
    this run (a scratch beside `cwd`, destroyed with the snapshot), and every
    file in `mutated_paths` has its cached bytecode under that prefix evicted
    before this launch: without that, a mutant can be graded against the
    unmutated baseline's cached bytecode and report a survivor (or a kill)
    that no source diff explains. The Python adapter passes the campaign's
    mutated sources; the C++ and Rust adapters mutate files with no bytecode
    to cache, so theirs stay empty.
    """

    captured: list[str] = []
    result = _support.run_command(
        argv,
        cwd=cwd,
        timeout_seconds=timeout_seconds,
        environment=isolated_python_env(
            environment, scratch_root_for(cwd), mutated_paths=mutated_paths
        ),
        pin_argv=list,
        result_factory=CommandResult,
        output_tail_chars=OUTPUT_TAIL_CHARS,
        stdout_sink=captured.append,
    )
    return result, captured[0] if captured else ""


def pinned(argv: Sequence[str], interpreter: str) -> list[str]:
    """Resolve a bare `python` argv[0] to the interpreter the receipt pins."""

    if argv and argv[0] in ("python", "python3"):
        return [interpreter, *argv[1:]]
    return list(argv)


def resolve_mutant_timeout(
    campaign: GeneratedCampaign, baseline_duration_seconds: float
) -> int:
    """Pin the per-mutant bound onto the campaign, from the manifest or the baseline.

    A manifest's `mutant_timeout_seconds` wins outright -- a lane that knows its
    suite is slow says so once. Otherwise the bound is three times the baseline
    suite's wall time with a 60 s floor, resolved here because only the adapter,
    having just run that baseline, knows the number. The resolved value lands on
    the campaign (adapters build their engine flags from the field) and the
    caller writes it into the receipt, so a reader can tell a pinned bound from
    a derived one by diffing against the manifest.
    """

    if campaign.mutant_timeout_seconds is not None:
        return campaign.mutant_timeout_seconds
    derived = max(
        MUTANT_TIMEOUT_FLOOR_SECONDS,
        math.ceil(MUTANT_TIMEOUT_BASELINE_MULTIPLIER * baseline_duration_seconds),
    )
    campaign.mutant_timeout_seconds = derived
    return derived


def snapshot_python_environment() -> dict[str, str]:
    """The interpreter export that lets a snapshot's tests drive Python.

    A git snapshot carries no ``.venv`` (it is gitignored), so a Rust suite
    that shells out to Python resolves ``python3`` from PATH and cannot import
    ``conductor`` -- the campaign dies at its own baseline inside a snapshot
    that passes outside it. The adapters merge this into every environment
    they build; the Rust harness ranks ``CONDUCTOR_SNAPSHOT_PYTHON`` ahead of
    its venv-over-PATH rule, so inside a sandbox the export is the only
    interpreter that works and outside one it never fires.
    """

    return {"CONDUCTOR_SNAPSHOT_PYTHON": snapshot_python(REPO_ROOT)}


# What a Python-less snapshot looks like from the baseline's own output. The
# point is not to diagnose every failure -- it is to stop the one failure mode
# that reads as a mystery ("baseline failed in the snapshot, passes on the
# host") from being a mystery.
_INTERPRETER_FAILURE_MARKERS = (
    "python3: not found",
    "ModuleNotFoundError",
    "No module named",
)


def _missing_interpreter_hint(baseline: CommandResult) -> str | None:
    """Why this baseline may have died for want of a Python interpreter.

    Falls off the end with None rather than saying `return None`: fest's
    rewrite of the None constant is a no-op, so an explicit None is an
    unkillable mutant that would redden the ratchet forever.
    """

    recorded = baseline.as_dict()
    tails = " ".join(
        str(recorded.get(key) or "") for key in ("stdout_tail", "stderr_tail")
    )
    for marker in _INTERPRETER_FAILURE_MARKERS:
        if marker in tails:
            return (
                "the baseline could not drive Python inside the snapshot: "
                "export CONDUCTOR_SNAPSHOT_PYTHON (the engine adapters do "
                "this automatically; [tool.conductor].snapshot_python "
                "overrides which interpreter)"
            )


def note_baseline(
    campaign: GeneratedCampaign,
    receipt: dict[str, Any],
    baseline: CommandResult,
    baseline_argv: Sequence[str],
    output_path: Path,
) -> None:
    """Record one adapter's baseline run, then pin the per-mutant bound from it.

    Every adapter's baseline means the same three things, in the same order:
    the receipt names the command that ran, a baseline that failed or hung is
    a refused campaign rather than a scored one, and only a green baseline can
    supply the wall time a derived bound needs -- so the resolution belongs
    here, once, and not three times in the adapters.
    """

    receipt["baseline_argv"] = list(baseline_argv)
    receipt["baseline"] = baseline.as_dict()
    if baseline.timed_out or baseline.returncode != 0:
        receipt["status"] = "BASELINE_FAILED"
        write_receipt(output_path, receipt)
        hint = _missing_interpreter_hint(baseline)
        detail = f"unmutated baseline failed; receipt={output_path}"
        raise CampaignError(f"{detail}; {hint}" if hint else detail)
    receipt["mutant_timeout_seconds"] = resolve_mutant_timeout(
        campaign, baseline.duration_seconds
    )


def require_executed(generated: int, tested: int, source: Sequence[str]) -> None:
    """Refuse a run that scored nothing, however green it looks.

    A campaign whose globs match no file, or whose coverage map never reaches
    the mutated file, produces zero executed mutants and therefore zero
    survivors -- which scores as a clean pass. That is how a campaign goes
    vacuously green, and it is the exact defect this engine exists to remove.
    """

    if not generated:
        raise CampaignError(
            "the engine generated no mutants; source globs matched nothing: "
            f"{list(source)}"
        )
    if not tested:
        raise CampaignError(
            f"no mutant was executed: all {generated} were reported as unreached "
            "or unbuildable, so no test ever ran against a mutation"
        )


def require_scored_rows(campaign: GeneratedCampaign, receipt: dict[str, Any]) -> None:
    """`require_executed`, counted from receipt rows instead of engine totals.

    cargo-mutants and mull report per-mutant rows rather than summary counters,
    so both adapters count the outcomes that score straight off the receipt.
    The two copies had grown into a duplication-gate clone pair, which is the
    signal that the sequence belongs here.
    """

    tested = sum(
        1 for row in receipt["mutants"] if row["outcome"] in (KILLED, SURVIVED)
    )
    require_executed(len(receipt["mutants"]), tested, campaign.source)


def open_receipt(
    campaign: GeneratedCampaign, repo_root: Path, adapter_path: Path
) -> dict[str, Any]:
    """The receipt envelope, in the shape the hand-written runner writes."""

    components = _runner_components_sha256()
    return {
        "schema_version": RECEIPT_SCHEMA,
        "campaign_id": campaign.campaign_id,
        "manifest": campaign.manifest_path.resolve()
        .relative_to(repo_root.resolve())
        .as_posix(),
        "manifest_sha256": campaign.manifest_sha256,
        "runner_sha256": components["conductor/mutation_testing.py"],
        "runner_components_sha256": components,
        "adapter_sha256": _sha256(adapter_path),
        "core_sha256": _sha256(Path(__file__)),
        "scope_guard_sha256": _sha256(
            Path(__file__).with_name("mutation_run_scope.py")
        ),
        "language": campaign.language,
        "mutation_engine": campaign.mutation_engine,
        "mutants_are_generated": True,
        "generated_at": datetime.now(UTC).isoformat(),
        "status": "RUNNING",
        "source_sha256": dict(campaign.source_sha256),
        "test_sha256": dict(campaign.test_sha256),
        "test_argv": list(campaign.test_argv),
        "interpreter": sys.executable,
        "seed": campaign.seed,
        "baseline": None,
        "mutants": [],
        "mutation_score": None,
        "survivors": [],
        "survivor_baseline": list(campaign.survivor_baseline),
        "new_survivors": [],
        "resolved_survivors": [],
        "no_coverage": None,
        "test_value": None,
        "killer_enforcement": None,
    }


def score(campaign: GeneratedCampaign, receipt: dict[str, Any]) -> None:
    """Score on the survivor SET, not on the fraction killed.

    A generated corpus has no curator, so its score is whatever the source
    happens to contain and comparing it to another campaign's means nothing.
    What does mean something is movement: a mutant that survives today and did
    not survive when the baseline was recorded is a test that stopped defending
    its code, and that is the only thing this campaign can fail on.
    """

    rows = receipt["mutants"]
    counts = dict.fromkeys(OUTCOMES, 0)
    for row in rows:
        if row["outcome"] not in counts:
            raise CampaignError(f"unknown mutant outcome {row['outcome']!r}")
        counts[row["outcome"]] += 1
    denominator = counts[KILLED] + counts[SURVIVED]
    receipt["outcome_counts"] = counts
    receipt["mutation_score"] = counts[KILLED] / denominator if denominator else None
    receipt["no_coverage"] = counts[NO_COVERAGE]
    receipt["unviable"] = counts[UNVIABLE]
    # Timed-out mutants are measurements the bound made, not failures the run
    # caused: the engine cut them off exactly as the campaign asked it to.
    # Reported beside the other non-scoring counts so a campaign that is merely
    # slow never reads as one that errored.
    receipt["timed_out"] = counts[TIMED_OUT]
    survivors = sorted(row["id"] for row in rows if row["outcome"] == SURVIVED)
    baseline = set(campaign.survivor_baseline)
    receipt["survivors"] = survivors
    receipt["new_survivors"] = sorted(set(survivors) - baseline)
    receipt["resolved_survivors"] = sorted(baseline - {*survivors})
    receipt["classification_required"] = list(receipt["new_survivors"])
    if counts[ERROR]:
        receipt["status"] = "ERROR"
    elif receipt["new_survivors"]:
        receipt["status"] = "FAIL"
    elif survivors:
        # Green would be a lie here: the recorded survivors are known gaps that
        # nobody has closed yet, not mutants somebody decided are harmless. The
        # gate holds -- nothing got worse -- and the debt stays legible.
        receipt["status"] = "RATCHET_HELD"
    else:
        receipt["status"] = "PASS"


MEMORY_CAP_ENV = "MUTATION_MEMORY_CAP_GIB"
DEFAULT_MEMORY_CAP_GIB = 8.0


def cap_memory() -> int | None:
    """Bound this run's memory, so a runaway engine cannot take the host down.

    Measured 2026-09-07: one generated run reached 41.5 GB of anonymous memory
    and was killed by the kernel OOM killer, which took the whole agent session
    with it. `RLIMIT_DATA` is inherited across fork and exec, so setting it here
    bounds this process, the engine binary, and every mutant test process the
    engine spawns -- and it counts private anonymous mappings only, so a
    device-backed CUDA reservation does not trip it. A lane that genuinely needs
    more has to say so: `MUTATION_MEMORY_CAP_GIB` raises it, `0` disables it.
    """

    raw = os.environ.get(MEMORY_CAP_ENV, str(DEFAULT_MEMORY_CAP_GIB))
    try:
        gib = float(raw)
    except ValueError as exc:
        raise CampaignError(f"{MEMORY_CAP_ENV} is not a number: {raw!r}") from exc
    if gib <= 0:
        return None
    limit = int(gib * 1024**3)
    _, hard = resource.getrlimit(resource.RLIMIT_DATA)
    if hard != resource.RLIM_INFINITY:
        limit = min(limit, hard)
    resource.setrlimit(resource.RLIMIT_DATA, (limit, hard))
    return limit


def record_survivor_baseline(
    campaign: GeneratedCampaign, receipt: dict[str, Any]
) -> bool:
    """Write a first run's survivor set into its own manifest, then re-score.

    This closes the loop that made every generated campaign permanently red: no
    baseline means every survivor is a new survivor. The tool records it, never
    an agent, so half (a) of the mutation-evidence grant holds -- the survivors
    are whatever the engine found, not a set anybody chose. It happens exactly
    once per campaign; afterwards the ratchet is live and a new survivor fails.
    """

    if campaign.survivor_baseline_recorded:
        return False
    if receipt["status"] == "ERROR":
        # A run that errored measured nothing; recording its survivors would
        # bless a gap that no test actually left open.
        return False
    payload = json.loads(campaign.manifest_path.read_text())
    payload["survivor_baseline"] = list(receipt["survivors"])
    payload["survivor_baseline_recorded"] = True
    payload["survivor_baseline_recorded_at"] = datetime.now(UTC).isoformat()
    payload.pop("survivor_baseline_note", None)
    _support.atomic_json(campaign.manifest_path, payload)
    campaign.survivor_baseline = tuple(sorted(receipt["survivors"]))
    campaign.survivor_baseline_recorded = True
    campaign.manifest_sha256 = _sha256(campaign.manifest_path)
    receipt["manifest_sha256"] = campaign.manifest_sha256
    receipt["survivor_baseline_recorded_by_this_run"] = True
    score(campaign, receipt)
    return True


def run_generated_campaign(
    campaign: GeneratedCampaign,
    *,
    allow_mutations: bool,
    receipt_path: Path | None = None,
    repo_root: Path = REPO_ROOT,
    adapter: EngineAdapter | None = None,
    owner: str | None = None,
    base: str = "origin/master",
    only: Sequence[str] = (),
) -> dict[str, Any]:
    """Generate, run and score a campaign's mutants inside a disposable snapshot."""

    if not allow_mutations:
        raise CampaignError("refusing mutation run without --allow-mutations")
    from conductor.mutation_run_scope import validate_run_scope

    validate_run_scope(campaign, repo_root=repo_root, owner=owner, base=base, only=only)
    engine = adapter or adapter_for(campaign.mutation_engine)
    binary = engine.binary()
    receipt = open_receipt(campaign, repo_root, Path(engine.__file__))
    receipt["memory_cap_bytes"] = cap_memory()
    output_path, relative = resolve_receipt_path(
        campaign, receipt_path, repo_root, generated_at=receipt["generated_at"]
    )
    write_receipt(output_path, receipt)
    try:
        with isolated_snapshot(repo_root) as snapshot:
            engine.execute(
                campaign,
                receipt,
                binary=binary,
                worktree=snapshot.worktree,
                output_path=output_path,
            )
    except CampaignError:
        if receipt["status"] == "RUNNING":
            receipt["status"] = "ERROR"
            write_receipt(output_path, receipt)
        raise
    except Exception as exc:
        receipt["status"] = "ERROR"
        receipt["error"] = f"{type(exc).__name__}: {exc}"
        write_receipt(output_path, receipt)
        raise CampaignError(f"generated campaign crashed: {exc}") from exc

    score(campaign, receipt)
    record_survivor_baseline(campaign, receipt)
    receipt["receipt_path"] = relative
    write_receipt(output_path, receipt)
    return receipt


def resolve_receipt_path(
    campaign: GeneratedCampaign,
    receipt_path: Path | None,
    repo_root: Path,
    *,
    generated_at: str,
) -> tuple[Path, str]:
    """Absolute output path and its repo-relative name, refusing anything outside.

    Absent an explicit ``receipt_path`` the default directory is host-configurable
    (``[tool.conductor].mutation_receipt_root``) and created on demand -- a host
    with no scratch-output tree of its own must not have to create one by hand.
    The filename's timestamp is parsed from ``generated_at`` -- the receipt's own
    stamp, already fixed by ``open_receipt`` -- rather than a second independent
    ``datetime.now(UTC)`` read; two clock reads a few lines apart drift on a host
    whose clock steps between them, landing a receipt whose filename names a
    different moment than the receipt itself claims (seen live as a
    `..._T999999Z.json` filename beside a normal `generated_at`).
    """

    if receipt_path is None:
        stamp = datetime.fromisoformat(generated_at).strftime("%Y%m%dT%H%M%SZ")
        directory = repo_root / mutation_receipt_root_relative(repo_root).as_posix()
        try:
            directory.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise CampaignError(
                f"cannot create mutation receipt directory {directory}: {exc}"
            ) from exc
        output = directory / f"{campaign.campaign_id}_{stamp}.json"
    else:
        output = (
            receipt_path if receipt_path.is_absolute() else repo_root / receipt_path
        ).resolve()
    try:
        return output, output.relative_to(repo_root.resolve()).as_posix()
    except ValueError as exc:
        raise CampaignError("receipt path must be inside the repository") from exc


def inspect_generated_campaign(
    campaign: GeneratedCampaign,
    *,
    repo_root: Path = REPO_ROOT,
    adapter: EngineAdapter | None = None,
) -> dict[str, Any]:
    """Report whether the campaign could run now, without running it."""

    reasons: list[str] = []
    try:
        engine = adapter or adapter_for(campaign.mutation_engine)
        binary: str | None = engine.binary()
    except CampaignError as exc:
        binary = None
        reasons.append(str(exc))
    for relative, expected in campaign.source_sha256.items():
        target = repo_root / relative
        if not target.is_file():
            reasons.append(f"pinned source is absent: {relative}")
        elif _sha256(target) != expected:
            reasons.append(f"pinned source drifted: {relative}")
    return {
        "campaign_id": campaign.campaign_id,
        "mutation_engine": campaign.mutation_engine,
        "engine_binary": binary,
        "source": list(campaign.source),
        "test_argv": list(campaign.test_argv),
        "survivor_baseline": list(campaign.survivor_baseline),
        "status": "READY" if not reasons else "NOT_READY",
        "readiness_reasons": reasons,
    }


def print_json(payload: Mapping[str, Any]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def main(argv: list[str] | None = None) -> int:
    """One CLI over every generated engine; the adapter comes from the manifest.

    Deliberately its own entry point rather than a branch inside
    `conductor/mutation_testing.py`. That module is a declared runner component:
    every receipt in the repository pins its hash, so adding four lines to it
    invalidates 29 test files' worth of evidence and demands the campaigns be
    re-run to say nothing new. One interface over both engines is worth having,
    but it costs a deliberate batch re-pin, not a drive-by edit.
    """

    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    inspect_parser = subparsers.add_parser(
        "inspect", help="validate a generated campaign without running it"
    )
    inspect_parser.add_argument("campaign", type=Path)
    run_parser = subparsers.add_parser(
        "run", help="generate, run and score mutants in an isolated snapshot"
    )
    run_parser.add_argument("campaign", type=Path)
    run_parser.add_argument("--allow-mutations", action="store_true")
    run_parser.add_argument("--receipt", type=Path)
    run_parser.add_argument("--owner", help="claim owner for dirty-file mutation scope")
    run_parser.add_argument("--base", default="origin/master")
    run_parser.add_argument("--only", action="append", default=[])
    args = parser.parse_args(argv)

    try:
        campaign = load_generated_campaign(args.campaign)
        if args.command == "inspect":
            result = inspect_generated_campaign(campaign)
            print_json(result)
            return 0 if result["status"] == "READY" else 3
        result = run_generated_campaign(
            campaign,
            allow_mutations=args.allow_mutations,
            receipt_path=args.receipt,
            repo_root=REPO_ROOT,
            owner=args.owner,
            base=args.base,
            only=args.only,
        )
        print_json(result)
        return 0 if result["status"] in ("PASS", "RATCHET_HELD") else 1
    except CampaignError as exc:
        print_json({"status": "REFUSED", "error": str(exc)})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
