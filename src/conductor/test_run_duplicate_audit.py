from __future__ import annotations

import fnmatch
import json
import subprocess
from pathlib import Path

import pytest

from conductor import run_duplicate_audit


CPD_NAMESPACE = "https://pmd-code.org/schema/cpd-report"


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    (repo / "research").mkdir()
    (repo / "conductor").mkdir()
    return repo


def _configure_jscpd(repo: Path, *, ignores: list[str] | None = None) -> None:
    binary = repo / "node_modules" / ".bin" / "jscpd"
    binary.parent.mkdir(parents=True)
    binary.write_text("#!/bin/sh\n", encoding="utf-8")
    (repo / "package.json").write_text(
        json.dumps({"jscpd": {"ignore": ignores or []}}) + "\n", encoding="utf-8"
    )
    (repo / ".gitignore").write_text("node_modules/\n", encoding="utf-8")
    _write_baseline(repo / run_duplicate_audit.JSCPD_BASELINE_RELATIVE)
    _git(
        repo,
        "add",
        "package.json",
        ".gitignore",
        run_duplicate_audit.JSCPD_BASELINE_RELATIVE.as_posix(),
    )


def _write_baseline(path: Path, entries: list[dict] | None = None) -> None:
    keyed = {
        entry["key"]: {
            "files": sorted((entry["firstFile"], entry["secondFile"])),
            "lines": entry["lines"],
        }
        for entry in (entries or [])
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "_comment": "test baseline",
                "count": len(keyed),
                "entries": keyed,
            }
        )
        + "\n",
        encoding="utf-8",
    )


def _completed(
    command: list[str], returncode: int = 0
) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(command, returncode, stdout="", stderr="")


def _jscpd_emulator(
    command: list[str], *, cwd: Path, tool_name: str
) -> subprocess.CompletedProcess[str]:
    assert tool_name == "jscpd"
    output = Path(command[command.index("--output") + 1])
    config = json.loads((cwd / "package.json").read_text(encoding="utf-8"))
    patterns = config.get("jscpd", {}).get("ignore", [])
    matches: list[Path] = []
    for source_dir in run_duplicate_audit.DEFAULT_SOURCE_DIRS:
        source_root = cwd / source_dir
        if not source_root.exists():
            continue
        for source in source_root.rglob("*"):
            relative = source.relative_to(cwd).as_posix()
            if source.is_file() and not any(
                fnmatch.fnmatchcase(relative, pattern) for pattern in patterns
            ):
                if "DUPLICATE_INDEX_SENTINEL" in source.read_text(encoding="utf-8"):
                    matches.append(source)
    duplicates = []
    if len(matches) >= 2:
        duplicates.append(
            {
                "firstFile": {"name": matches[0].relative_to(cwd).as_posix()},
                "secondFile": {"name": matches[1].relative_to(cwd).as_posix()},
                "fragment": "DUPLICATE_INDEX_SENTINEL = 1",
                "lines": 10,
            }
        )
    output.mkdir(parents=True, exist_ok=True)
    (output / "jscpd-report.json").write_text(
        json.dumps({"duplicates": duplicates}), encoding="utf-8"
    )
    return _completed(command)


def _pmd_emulator(
    command: list[str], *, cwd: Path, tool_name: str
) -> subprocess.CompletedProcess[str]:
    assert tool_name == "pmd-cpd"
    file_list = Path(command[command.index("--file-list") + 1])
    files = file_list.read_text(encoding="utf-8").splitlines()
    output = Path(command[command.index("--report-file") + 1])
    output.write_text(
        (
            f'<pmd-cpd xmlns="{CPD_NAMESPACE}">'
            '<duplication lines="10" tokens="80">'
            f'<file path="{files[0]}" line="1" endline="10" />'
            f'<file path="{files[1]}" line="1" endline="10" />'
            "<codefragment><![CDATA[DUPLICATE_INDEX_SENTINEL = 1]]></codefragment>"
            "</duplication></pmd-cpd>"
        ),
        encoding="utf-8",
    )
    return _completed(command)


def _snapshot_texts(command: list[str], cwd: object = None) -> list[str]:
    snapshot_dirs = [
        Path(argument)
        for argument in command
        if "llm-index-sources-" in argument and Path(argument).is_dir()
    ]
    if isinstance(cwd, Path) and "llm-index-sources-" in cwd.name:
        snapshot_dirs.extend(
            cwd / argument
            for argument in command
            if argument in run_duplicate_audit.DEFAULT_SOURCE_DIRS
        )
    return [
        path.read_text(encoding="utf-8")
        for directory in snapshot_dirs
        for path in directory.rglob("*")
        if path.is_file()
    ]


def test_materialized_sources_are_exact_index_blobs(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path)
    tracked = repo / "research" / "tracked.py"
    tracked.write_text("INDEX_VERSION = 1\n", encoding="utf-8")
    artifact = repo / "research" / "checkpoint.pt"
    artifact.write_bytes(b"tracked artifact must not be copied")
    _git(repo, "add", "research/tracked.py", "research/checkpoint.pt")

    tracked.write_text("WORKTREE_VERSION = 2\n", encoding="utf-8")
    (repo / "research" / "untracked.py").write_text(
        "UNTRACKED_VERSION = 3\n", encoding="utf-8"
    )

    with run_duplicate_audit.materialized_index_sources(
        ("research",), frozenset({".py"}), root=repo
    ) as snapshot:
        assert (snapshot / "research" / "tracked.py").read_text(
            encoding="utf-8"
        ) == "INDEX_VERSION = 1\n"
        assert not (snapshot / "research" / "untracked.py").exists()
        assert not (snapshot / "research" / "checkpoint.pt").exists()

    _git(repo, "add", "research/tracked.py", "research/untracked.py")
    with run_duplicate_audit.materialized_index_sources(
        ("research",), frozenset({".py"}), root=repo
    ) as snapshot:
        assert (snapshot / "research" / "tracked.py").read_text(
            encoding="utf-8"
        ) == "WORKTREE_VERSION = 2\n"
        assert (snapshot / "research" / "untracked.py").exists()


def test_vulture_check_ignores_untracked_but_fails_for_index_violation(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    (repo / "research" / "definition.py").write_text(
        "def shared_symbol():\n    return 1\n", encoding="utf-8"
    )
    (repo / "research" / "consumer.py").write_text(
        "from .definition import shared_symbol\nRESULT = shared_symbol()\n",
        encoding="utf-8",
    )
    _git(repo, "add", "research/definition.py", "research/consumer.py")
    violation = repo / "research" / "untracked_violation.py"
    violation.write_text("UNUSED_INDEX_SENTINEL = object()\n", encoding="utf-8")

    def vulture_detector(command: list[str], **kwargs: object) -> int:
        texts = _snapshot_texts(command, kwargs.get("cwd"))
        assert any("def shared_symbol" in text for text in texts)
        assert any("shared_symbol()" in text for text in texts)
        return int(any("UNUSED_INDEX_SENTINEL" in text for text in texts))

    monkeypatch.setattr(run_duplicate_audit, "run", vulture_detector)

    assert run_duplicate_audit.run_vulture(True, True, root=repo) == 0
    _git(repo, "add", "research/untracked_violation.py")
    assert run_duplicate_audit.run_vulture(True, True, root=repo) == 1


def test_jscpd_check_ignores_untracked_but_fails_for_index_duplicates(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    (repo / "research" / "base.py").write_text("VALUE = 1\n", encoding="utf-8")
    _git(repo, "add", "research/base.py")
    duplicate_a = repo / "research" / "untracked_a.py"
    duplicate_b = repo / "conductor" / "untracked_b.py"
    duplicate_a.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    duplicate_b.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")

    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _jscpd_emulator)

    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 0
    _git(repo, "add", "research/untracked_a.py", "conductor/untracked_b.py")
    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 1


def test_jscpd_snapshot_preserves_repository_relative_ignores(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo, ignores=["research/tests/**"])
    ignored = repo / "research" / "tests" / "ignored.py"
    ignored.parent.mkdir()
    ignored.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    ignored_peer = repo / "research" / "tests" / "ignored_peer.py"
    ignored_peer.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    _git(repo, "add", "research/tests/ignored.py", "research/tests/ignored_peer.py")

    # The candidate config comes from the index, not this unstaged worktree edit.
    (repo / "package.json").write_text(
        json.dumps({"jscpd": {"ignore": []}}) + "\n", encoding="utf-8"
    )

    def ignore_aware_detector(
        command: list[str], *, cwd: Path, tool_name: str
    ) -> subprocess.CompletedProcess[str]:
        assert "llm-index-sources-" in cwd.name
        assert not any(
            Path(argument).is_absolute()
            for argument in command
            if argument in run_duplicate_audit.DEFAULT_SOURCE_DIRS
        )
        config = json.loads((cwd / "package.json").read_text(encoding="utf-8"))
        patterns = config["jscpd"]["ignore"]
        for source_dir in run_duplicate_audit.DEFAULT_SOURCE_DIRS:
            source_root = cwd / source_dir
            if not source_root.exists():
                continue
            for source in source_root.rglob("*"):
                relative = source.relative_to(cwd).as_posix()
                if source.is_file() and not any(
                    fnmatch.fnmatchcase(relative, pattern) for pattern in patterns
                ):
                    assert "DUPLICATE_INDEX_SENTINEL" not in source.read_text(
                        encoding="utf-8"
                    )
        return _jscpd_emulator(command, cwd=cwd, tool_name=tool_name)

    monkeypatch.setattr(
        run_duplicate_audit, "_run_report_command", ignore_aware_detector
    )
    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 0


@pytest.mark.parametrize("failure", ["nonzero", "missing", "malformed"])
def test_jscpd_report_failures_are_blocking(
    tmp_path: Path, monkeypatch, failure: str
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    (repo / "research" / "source.py").write_text("VALUE = 1\n", encoding="utf-8")

    def fail_report(
        command: list[str], *, cwd: Path, tool_name: str
    ) -> subprocess.CompletedProcess[str]:
        del cwd
        assert tool_name == "jscpd"
        if failure == "nonzero":
            raise run_duplicate_audit.DuplicateAuditError(
                "jscpd exited 9; report rejected"
            )
        if failure == "malformed":
            output = Path(command[command.index("--output") + 1])
            output.mkdir(parents=True, exist_ok=True)
            (output / "jscpd-report.json").write_text("{", encoding="utf-8")
        return _completed(command)

    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", fail_report)
    assert (
        run_duplicate_audit.run_jscpd(check=True, root=repo)
        == run_duplicate_audit.AUDIT_ERROR_EXIT_CODE
    )


@pytest.mark.parametrize("failure", ["nonzero", "missing", "malformed"])
def test_pmd_report_failures_are_blocking(
    tmp_path: Path, monkeypatch, failure: str
) -> None:
    repo = _init_repo(tmp_path)
    _write_baseline(repo / run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE)
    (repo / "research" / "source.py").write_text("VALUE = 1\n", encoding="utf-8")

    def fail_report(
        command: list[str], *, cwd: Path, tool_name: str
    ) -> subprocess.CompletedProcess[str]:
        del cwd
        assert tool_name == "pmd-cpd"
        if failure == "nonzero":
            raise run_duplicate_audit.DuplicateAuditError(
                "pmd-cpd exited 9; report rejected"
            )
        if failure == "malformed":
            report = Path(command[command.index("--report-file") + 1])
            report.write_text("<pmd-cpd>", encoding="utf-8")
        return _completed(command)

    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", fail_report)
    assert (
        run_duplicate_audit.run_pmd_python(check=True, root=repo)
        == run_duplicate_audit.AUDIT_ERROR_EXIT_CODE
    )


@pytest.mark.parametrize("tool_name", ["jscpd", "pmd-cpd"])
def test_report_command_rejects_nonzero_exit(
    tmp_path: Path, monkeypatch, tool_name: str
) -> None:
    monkeypatch.setattr(
        subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(
            args[0], 9, stdout="", stderr="analyzer crashed"
        ),
    )

    with pytest.raises(
        run_duplicate_audit.DuplicateAuditError,
        match=f"{tool_name} exited 9; report rejected: analyzer crashed",
    ):
        run_duplicate_audit._run_report_command(
            ["analyzer"], cwd=tmp_path, tool_name=tool_name
        )


@pytest.mark.parametrize(
    "payload",
    [
        {"_comment": "test", "count": 1, "entries": {}},
        {
            "_comment": "test",
            "count": 1,
            "entries": {"not-a-clone-key": {"files": ["a.py", "b.py"], "lines": 10}},
        },
    ],
)
def test_baseline_count_and_entry_keys_are_validated(
    tmp_path: Path, payload: dict
) -> None:
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(payload), encoding="utf-8")

    assert (
        run_duplicate_audit._check_against_baseline(
            baseline, [], tool_name="test-analyzer"
        )
        == run_duplicate_audit.AUDIT_ERROR_EXIT_CODE
    )


def test_jscpd_index_check_reads_staged_baseline(tmp_path: Path, monkeypatch) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    first = repo / "research" / "first.py"
    second = repo / "conductor" / "second.py"
    fragment = "DUPLICATE_INDEX_SENTINEL = 1"
    first.write_text(fragment + "\n", encoding="utf-8")
    second.write_text(fragment + "\n", encoding="utf-8")
    _git(repo, "add", "research/first.py", "conductor/second.py")
    entry = {
        "key": run_duplicate_audit._stable_dup_key(
            "research/first.py", "conductor/second.py", fragment
        ),
        "firstFile": "research/first.py",
        "secondFile": "conductor/second.py",
        "lines": 10,
    }
    baseline = repo / run_duplicate_audit.JSCPD_BASELINE_RELATIVE
    _write_baseline(baseline, [entry])
    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _jscpd_emulator)

    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 1
    _git(repo, "add", run_duplicate_audit.JSCPD_BASELINE_RELATIVE.as_posix())
    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 0


def test_pmd_index_check_reads_staged_baseline(tmp_path: Path, monkeypatch) -> None:
    repo = _init_repo(tmp_path)
    first = repo / "research" / "first.py"
    second = repo / "conductor" / "second.py"
    fragment = "DUPLICATE_INDEX_SENTINEL = 1"
    first.write_text(fragment + "\n", encoding="utf-8")
    second.write_text(fragment + "\n", encoding="utf-8")
    baseline = repo / run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE
    _write_baseline(baseline)
    _git(
        repo,
        "add",
        "research/first.py",
        "conductor/second.py",
        run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE.as_posix(),
    )
    entry = {
        "key": run_duplicate_audit._stable_dup_key(
            "research/first.py", "conductor/second.py", fragment
        ),
        "firstFile": "research/first.py",
        "secondFile": "conductor/second.py",
        "lines": 10,
    }
    _write_baseline(baseline, [entry])
    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _pmd_emulator)

    assert run_duplicate_audit.run_pmd_python(True, True, root=repo) == 1
    _git(repo, "add", run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE.as_posix())
    assert run_duplicate_audit.run_pmd_python(True, True, root=repo) == 0
