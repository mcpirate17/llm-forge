"""Execute the Rust-planned candidate-local build for selected native contracts."""

from __future__ import annotations

import json
import os
import shutil
import site
import subprocess
import sys
import sysconfig
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import TypedDict

from conductor._native import candidate_verification_native
from conductor.candidate_review.checks import ContractPlan, ReviewContext
from conductor.candidate_review.command_runner import _run_process, _tail
from conductor.candidate_review.policy import CheckPolicy


class RuntimeBuild(TypedDict):
    cwd: str
    argv: list[str]
    artifact: str
    destination: str


class RuntimePlan(TypedDict):
    build_commands: list[RuntimeBuild]
    test_env: dict[str, str]
    extension: str


def _runtime_plan(
    snapshot: Path, runtime_dir: Path, contract_plan: ContractPlan
) -> RuntimePlan:
    sites = {sysconfig.get_path("purelib"), sysconfig.get_path("platlib")}
    sites.update(site.getsitepackages())
    request = {
        "snapshot": str(snapshot),
        "runtime_dir": str(runtime_dir),
        "python_executable": sys.executable,
        "python_sites": sorted(path for path in sites if path),
        "python_libdir": sysconfig.get_config_var("LIBDIR"),
        "ld_library_path": os.environ.get("LD_LIBRARY_PATH", ""),
        "targets": contract_plan["targets"],
        "SQLITE3_LIB_DIR": os.environ.get("SQLITE3_LIB_DIR"),
        "RUSTUP_TOOLCHAIN": os.environ.get("RUSTUP_TOOLCHAIN"),
    }
    return json.loads(
        candidate_verification_native("contract_runtime_plan", json.dumps(request))
    )


def _stage_artifact(build: RuntimeBuild) -> None:
    artifact = Path(build["artifact"])
    destination = Path(build["destination"])
    if not artifact.is_file():
        raise RuntimeError(f"candidate contract build omitted {artifact}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(artifact, destination)


def prepare_contract_runtime(
    ctx: ReviewContext, check: CheckPolicy, contract_plan: ContractPlan
) -> dict[str, str]:
    """Build and stage candidate binaries under the review's isolated runtime."""
    runtime = _runtime_plan(ctx.snapshot, ctx.runtime_dir, contract_plan)
    environment: dict[str, str] = runtime["test_env"]
    for build in runtime["build_commands"]:
        if Path(build["cwd"]) != ctx.snapshot:
            raise RuntimeError("native contract build escaped the candidate snapshot")
        try:
            completed = _run_process(
                build["argv"],
                ctx=ctx,
                timeout_seconds=check.timeout_seconds,
                memory_mb=check.memory_mb,
                include_git_metadata=False,
                extra_env=environment,
                wall_timeout_seconds=check.wall_timeout_seconds,
            )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(
                f"candidate contract build exceeded {check.wall_timeout_seconds}s wall budget"
            ) from exc
        if completed.returncode:
            detail = _tail(
                completed.stdout + "\n" + completed.stderr, check.max_output_chars
            ).strip()
            raise RuntimeError(
                f"candidate contract build exited {completed.returncode}: {detail}"
            )
        _stage_artifact(build)
    return environment


@contextmanager
def standalone_contract_runtime(
    repo: Path, contract_plan: ContractPlan
) -> Iterator[dict[str, str]]:
    """Stage the same candidate binaries for one standalone graph-selector run."""
    with tempfile.TemporaryDirectory(prefix="forge-graph-contracts-") as temporary:
        runtime = _runtime_plan(repo, Path(temporary), contract_plan)
        environment: dict[str, str] = runtime["test_env"]
        for build in runtime["build_commands"]:
            if Path(build["cwd"]) != repo:
                raise RuntimeError(
                    "native contract build escaped the selected repository"
                )
            completed = subprocess.run(
                build["argv"],
                cwd=repo,
                env={
                    **{
                        key: value
                        for key, value in os.environ.items()
                        if key not in {"PYO3_CONFIG_FILE", "PYTHONHOME"}
                    },
                    **environment,
                },
                capture_output=True,
                text=True,
                timeout=900,
                check=False,
            )
            if completed.returncode:
                detail = _tail(completed.stdout + "\n" + completed.stderr, 10000)
                raise RuntimeError(
                    f"contract build exited {completed.returncode}: {detail.strip()}"
                )
            _stage_artifact(build)
        yield environment
