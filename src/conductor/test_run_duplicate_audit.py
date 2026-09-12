from __future__ import annotations

import fnmatch
import json
import subprocess
import sys
from pathlib import Path

import pytest

from conductor import run_duplicate_audit

CPD_NAMESPACE = "https://pmd-code.org/schema/cpd-report"


def _stable_dup_key(first_path: str, second_path: str, fragment: str) -> str:
    """Content-hash identity for a clone pair, as the baseline stores it.

    Production gets the key from ``normalize_jscpd_report_native``; the Python
    reimplementation that shipped in ``run_duplicate_audit`` had no caller and
    moved here on 2026-09-06, where its only job is to let these tests stub
    ``_jscpd_collect_duplicates`` with entries the baseline round-trip accepts.
    """
    normalized = "\n".join(line.rstrip() for line in fragment.strip("\n").splitlines())
    from conductor._native import stable_duplicate_key_native

    return stable_duplicate_key_native(
        first_path,
        second_path,
        normalized.encode("utf-8", "surrogateescape"),
    )


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    (repo / "src").mkdir()
    (repo / "native").mkdir()
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


def _configure_pmd(repo: Path) -> None:
    """Give the repo a resolvable ``pmd`` so ``_resolve_pmd_executable`` succeeds.

    Mirrors ``_configure_jscpd``: a project-local ``node_modules/.bin/pmd`` is
    the first thing ``_resolve_pmd_executable`` checks, so tests that monkeypatch
    ``_run_report_command`` never need a real PMD install on PATH.
    """
    binary = repo / "node_modules" / ".bin" / "pmd"
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text("#!/bin/sh\n", encoding="utf-8")


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
            if (
                source.is_file()
                and not any(
                    fnmatch.fnmatchcase(relative, pattern) for pattern in patterns
                )
                and "DUPLICATE_INDEX_SENTINEL" in source.read_text(encoding="utf-8")
            ):
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


def test_resolve_audit_root_explicit_path_wins(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path)
    explicit = tmp_path / "exported-candidate"
    explicit.mkdir()

    assert (
        run_duplicate_audit._resolve_audit_root(Path("..") / explicit.name, cwd=repo)
        == explicit.resolve()
    )


def test_resolve_audit_root_fails_closed_outside_git(tmp_path: Path) -> None:
    outside = tmp_path / "not-a-worktree"
    outside.mkdir()

    with pytest.raises(
        run_duplicate_audit.DuplicateAuditError,
        match="cannot resolve audit root.*pass --root explicitly",
    ):
        run_duplicate_audit._resolve_audit_root(None, cwd=outside)


def test_main_passes_cwd_git_root_to_selected_tool(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    repo = _init_repo(tmp_path)
    seen: dict[str, object] = {}

    def fake_jscpd(
        check: bool,
        index_snapshot: bool,
        save_baseline: bool,
        *,
        root: Path,
        changed_files: frozenset[str] | None = None,
    ) -> int:
        seen.update(
            check=check,
            index_snapshot=index_snapshot,
            save_baseline=save_baseline,
            root=root,
            changed_files=changed_files,
        )
        return 0

    monkeypatch.chdir(repo)
    monkeypatch.setitem(run_duplicate_audit.TOOLS, "jscpd", fake_jscpd)
    monkeypatch.setattr(
        sys,
        "argv",
        ["run_duplicate_audit", "--tool", "jscpd", "--check"],
    )

    assert run_duplicate_audit.main() == 0
    assert seen == {
        "check": True,
        "index_snapshot": False,
        "save_baseline": False,
        "root": repo.resolve(),
        "changed_files": None,
    }
    output = capsys.readouterr().out
    assert f"audit-root: {repo.resolve()}" in output
    assert "mode: worktree" in output


def test_main_threads_changed_file_cli_flags_to_baseline_supported_tool(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """--changed-file (repeatable) and --changed-files-from both reach the
    tool as one merged frozenset; a tool outside baseline_supported (nicad
    -python) never receives changed_files at all."""
    repo = _init_repo(tmp_path)
    changed_files_from = tmp_path / "changed.txt"
    changed_files_from.write_text("c/three.py\n", encoding="utf-8")
    seen: dict[str, object] = {}

    def fake_pmd(
        check: bool,
        index_snapshot: bool,
        save_baseline: bool,
        *,
        root: Path,
        changed_files: frozenset[str] | None = None,
    ) -> int:
        seen["pmd-python"] = changed_files
        return 0

    def fake_nicad(
        check: bool,
        index_snapshot: bool,
        save_baseline: bool,
        *,
        root: Path,
    ) -> int:
        seen["nicad-python"] = "called-without-changed-files-kwarg"
        return 0

    monkeypatch.chdir(repo)
    monkeypatch.setitem(run_duplicate_audit.TOOLS, "pmd-python", fake_pmd)
    monkeypatch.setitem(run_duplicate_audit.TOOLS, "nicad-python", fake_nicad)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_duplicate_audit",
            "--tool",
            "pmd-python",
            "--tool",
            "nicad-python",
            "--check",
            "--changed-file",
            "a/one.py",
            "--changed-files-from",
            str(changed_files_from),
        ],
    )

    assert run_duplicate_audit.main() == 0
    assert seen["pmd-python"] == frozenset({"a/one.py", "c/three.py"})
    assert seen["nicad-python"] == "called-without-changed-files-kwarg"


def test_jscpd_live_scan_uses_git_visible_sources(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    nested_ignore = repo / "src" / ".gitignore"
    nested_ignore.write_text("reports/\n", encoding="utf-8")
    _git(repo, "add", "src/.gitignore")

    ignored_dir = repo / "src" / "reports"
    ignored_dir.mkdir()
    ignored = [ignored_dir / "ignored_a.py", ignored_dir / "ignored_b.py"]
    visible = [repo / "src" / "visible_a.py", repo / "native" / "visible_b.py"]
    for path in [*ignored, *visible]:
        path.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")

    seen: list[str] = []

    def fake_collect(
        paths: list[str], *, cwd: Path, executable: str | None = None
    ) -> list[dict]:
        del executable
        for source_dir in paths:
            seen.extend(
                path.relative_to(cwd).as_posix()
                for path in (cwd / source_dir).rglob("*")
                if path.is_file()
            )
        first, second = [path.relative_to(repo).as_posix() for path in visible]
        fragment = "DUPLICATE_INDEX_SENTINEL = 1"
        return [
            {
                "key": _stable_dup_key(first, second, fragment),
                "firstFile": first,
                "secondFile": second,
                "lines": 10,
            }
        ]

    monkeypatch.setattr(run_duplicate_audit, "_jscpd_collect_duplicates", fake_collect)

    assert run_duplicate_audit.run_jscpd(check=True, root=repo) == 1
    assert {path.relative_to(repo).as_posix() for path in visible} <= set(seen)
    assert not ({path.relative_to(repo).as_posix() for path in ignored} & set(seen))


def test_materialized_sources_are_exact_index_blobs(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path)
    tracked = repo / "src" / "tracked.py"
    tracked.write_text("INDEX_VERSION = 1\n", encoding="utf-8")
    artifact = repo / "src" / "checkpoint.pt"
    artifact.write_bytes(b"tracked artifact must not be copied")
    _git(repo, "add", "src/tracked.py", "src/checkpoint.pt")

    tracked.write_text("WORKTREE_VERSION = 2\n", encoding="utf-8")
    (repo / "src" / "untracked.py").write_text(
        "UNTRACKED_VERSION = 3\n", encoding="utf-8"
    )

    with run_duplicate_audit.materialized_index_sources(
        ("src",), frozenset({".py"}), root=repo
    ) as snapshot:
        assert (snapshot / "src" / "tracked.py").read_text(
            encoding="utf-8"
        ) == "INDEX_VERSION = 1\n"
        assert not (snapshot / "src" / "untracked.py").exists()
        assert not (snapshot / "src" / "checkpoint.pt").exists()

    _git(repo, "add", "src/tracked.py", "src/untracked.py")
    with run_duplicate_audit.materialized_index_sources(
        ("src",), frozenset({".py"}), root=repo
    ) as snapshot:
        assert (snapshot / "src" / "tracked.py").read_text(
            encoding="utf-8"
        ) == "WORKTREE_VERSION = 2\n"
        assert (snapshot / "src" / "untracked.py").exists()


def test_vulture_check_ignores_untracked_but_fails_for_index_violation(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    (repo / "src" / "definition.py").write_text(
        "def shared_symbol():\n    return 1\n", encoding="utf-8"
    )
    (repo / "src" / "consumer.py").write_text(
        "from .definition import shared_symbol\nRESULT = shared_symbol()\n",
        encoding="utf-8",
    )
    _git(repo, "add", "src/definition.py", "src/consumer.py")
    violation = repo / "src" / "untracked_violation.py"
    violation.write_text("UNUSED_INDEX_SENTINEL = object()\n", encoding="utf-8")

    def vulture_detector(command: list[str], **kwargs: object) -> int:
        texts = _snapshot_texts(command, kwargs.get("cwd"))
        assert any("def shared_symbol" in text for text in texts)
        assert any("shared_symbol()" in text for text in texts)
        return int(any("UNUSED_INDEX_SENTINEL" in text for text in texts))

    monkeypatch.setattr(run_duplicate_audit, "run", vulture_detector)

    assert run_duplicate_audit.run_vulture(True, True, root=repo) == 0
    _git(repo, "add", "src/untracked_violation.py")
    assert run_duplicate_audit.run_vulture(True, True, root=repo) == 1


def test_jscpd_check_ignores_untracked_but_fails_for_index_duplicates(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    (repo / "src" / "base.py").write_text("VALUE = 1\n", encoding="utf-8")
    _git(repo, "add", "src/base.py")
    duplicate_a = repo / "src" / "untracked_a.py"
    duplicate_b = repo / "native" / "untracked_b.py"
    duplicate_a.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    duplicate_b.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")

    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _jscpd_emulator)

    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 0
    _git(repo, "add", "src/untracked_a.py", "native/untracked_b.py")
    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 1


def test_jscpd_snapshot_preserves_repository_relative_ignores(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo, ignores=["src/tests/**"])
    ignored = repo / "src" / "tests" / "ignored.py"
    ignored.parent.mkdir()
    ignored.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    ignored_peer = repo / "src" / "tests" / "ignored_peer.py"
    ignored_peer.write_text("DUPLICATE_INDEX_SENTINEL = 1\n", encoding="utf-8")
    _git(repo, "add", "src/tests/ignored.py", "src/tests/ignored_peer.py")

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


def _make_fail_report(*, expected_tool_name: str, failure: str, corrupt_report):
    """Build a ``_run_report_command`` double that injects one failure mode.

    Shared by the jscpd and PMD "report failures are blocking" tests below --
    both drive the same three failure modes through the same seam, differing
    only in the tool name asserted and how a malformed report gets written.
    """

    def fail_report(
        command: list[str], *, cwd: Path, tool_name: str
    ) -> subprocess.CompletedProcess[str]:
        del cwd
        assert tool_name == expected_tool_name
        if failure == "nonzero":
            raise run_duplicate_audit.DuplicateAuditError(
                f"{tool_name} exited 9; report rejected"
            )
        if failure == "malformed":
            corrupt_report(command)
        return _completed(command)

    return fail_report


@pytest.mark.parametrize("failure", ["nonzero", "missing", "malformed"])
def test_jscpd_report_failures_are_blocking(
    tmp_path: Path, monkeypatch, failure: str
) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    (repo / "src" / "source.py").write_text("VALUE = 1\n", encoding="utf-8")

    def corrupt_report(command: list[str]) -> None:
        output = Path(command[command.index("--output") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "jscpd-report.json").write_text("{", encoding="utf-8")

    monkeypatch.setattr(
        run_duplicate_audit,
        "_run_report_command",
        _make_fail_report(
            expected_tool_name="jscpd", failure=failure, corrupt_report=corrupt_report
        ),
    )
    assert (
        run_duplicate_audit.run_jscpd(check=True, root=repo)
        == run_duplicate_audit.AUDIT_ERROR_EXIT_CODE
    )


@pytest.mark.parametrize("failure", ["nonzero", "missing", "malformed"])
def test_pmd_report_failures_are_blocking(
    tmp_path: Path, monkeypatch, failure: str
) -> None:
    repo = _init_repo(tmp_path)
    _configure_pmd(repo)
    _write_baseline(repo / run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE)
    (repo / "src" / "source.py").write_text("VALUE = 1\n", encoding="utf-8")

    def corrupt_report(command: list[str]) -> None:
        report = Path(command[command.index("--report-file") + 1])
        report.write_text("<pmd-cpd>", encoding="utf-8")

    monkeypatch.setattr(
        run_duplicate_audit,
        "_run_report_command",
        _make_fail_report(
            expected_tool_name="pmd-cpd", failure=failure, corrupt_report=corrupt_report
        ),
    )
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


def _dup_entry(first: str, second: str, fragment: str, *, lines: int = 10) -> dict:
    return {
        "key": _stable_dup_key(first, second, fragment),
        "firstFile": first,
        "secondFile": second,
        "lines": lines,
    }


def test_check_against_baseline_without_changed_files_blocks_every_new_pair(
    tmp_path: Path,
) -> None:
    """Legacy mode (no --changed-file at all): every new pair blocks, as today."""
    baseline = tmp_path / "baseline.json"
    _write_baseline(baseline)
    left = _dup_entry("a/one.py", "b/two.py", "LEFT_SENTINEL = 1")
    right = _dup_entry("c/three.py", "d/four.py", "RIGHT_SENTINEL = 1")

    exit_code = run_duplicate_audit._check_against_baseline(
        baseline, [left, right], tool_name="test-analyzer"
    )

    assert exit_code == 1


def test_check_against_baseline_caused_via_left_side_blocks(tmp_path: Path) -> None:
    baseline = tmp_path / "baseline.json"
    _write_baseline(baseline)
    entry = _dup_entry("a/one.py", "b/two.py", "LEFT_SENTINEL = 1")

    exit_code = run_duplicate_audit._check_against_baseline(
        baseline,
        [entry],
        tool_name="test-analyzer",
        changed_files=frozenset({"a/one.py"}),
    )

    assert exit_code == 1


def test_check_against_baseline_caused_via_right_side_blocks(tmp_path: Path) -> None:
    baseline = tmp_path / "baseline.json"
    _write_baseline(baseline)
    entry = _dup_entry("a/one.py", "b/two.py", "RIGHT_SENTINEL = 1")

    exit_code = run_duplicate_audit._check_against_baseline(
        baseline,
        [entry],
        tool_name="test-analyzer",
        changed_files=frozenset({"b/two.py"}),
    )

    assert exit_code == 1


def test_check_against_baseline_inherited_via_neither_side_does_not_block(
    tmp_path: Path,
) -> None:
    baseline = tmp_path / "baseline.json"
    _write_baseline(baseline)
    entry = _dup_entry("a/one.py", "b/two.py", "NEITHER_SENTINEL = 1")

    exit_code = run_duplicate_audit._check_against_baseline(
        baseline,
        [entry],
        tool_name="test-analyzer",
        changed_files=frozenset({"z/unrelated.py"}),
    )

    assert exit_code == 0


def test_check_against_baseline_no_new_findings_exits_zero_either_way(
    tmp_path: Path,
) -> None:
    baseline = tmp_path / "baseline.json"
    entry = _dup_entry("a/one.py", "b/two.py", "ALREADY_KNOWN = 1")
    _write_baseline(baseline, [entry])

    assert (
        run_duplicate_audit._check_against_baseline(
            baseline, [entry], tool_name="test-analyzer", changed_files=None
        )
        == 0
    )
    assert (
        run_duplicate_audit._check_against_baseline(
            baseline,
            [entry],
            tool_name="test-analyzer",
            changed_files=frozenset({"a/one.py"}),
        )
        == 0
    )


def test_check_against_baseline_changed_baseline_only_file_not_reported_as_caused(
    tmp_path: Path,
) -> None:
    """A --changed-file naming a *baseline* (non-new) pair must not spuriously
    attribute an unrelated NEW pair to the candidate."""
    baseline_only = _dup_entry("a/one.py", "b/two.py", "ALREADY_KNOWN = 1")
    baseline = tmp_path / "baseline.json"
    _write_baseline(baseline, [baseline_only])
    unrelated_new = _dup_entry("c/three.py", "d/four.py", "BRAND_NEW = 1")

    exit_code = run_duplicate_audit._check_against_baseline(
        baseline,
        [baseline_only, unrelated_new],
        tool_name="test-analyzer",
        # "a/one.py" only ever appears in the baseline-only pair, never in a
        # NEW one -- it must not cause unrelated_new to be marked CAUSED.
        changed_files=frozenset({"a/one.py"}),
    )

    assert exit_code == 0


def test_jscpd_index_check_reads_staged_baseline(tmp_path: Path, monkeypatch) -> None:
    repo = _init_repo(tmp_path)
    _configure_jscpd(repo)
    first = repo / "src" / "first.py"
    second = repo / "native" / "second.py"
    fragment = "DUPLICATE_INDEX_SENTINEL = 1"
    first.write_text(fragment + "\n", encoding="utf-8")
    second.write_text(fragment + "\n", encoding="utf-8")
    _git(repo, "add", "src/first.py", "native/second.py")
    entry = {
        "key": _stable_dup_key("src/first.py", "native/second.py", fragment),
        "firstFile": "src/first.py",
        "secondFile": "native/second.py",
        "lines": 10,
    }
    baseline = repo / run_duplicate_audit.JSCPD_BASELINE_RELATIVE
    _write_baseline(baseline, [entry])
    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _jscpd_emulator)

    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 1
    _git(repo, "add", run_duplicate_audit.JSCPD_BASELINE_RELATIVE.as_posix())
    assert run_duplicate_audit.run_jscpd(True, True, root=repo) == 0


def test_resolve_pmd_executable_prefers_an_explicit_override(tmp_path: Path) -> None:
    repo = _init_repo(tmp_path)
    _configure_pmd(repo)
    assert (
        run_duplicate_audit._resolve_pmd_executable(repo, "/custom/pmd")
        == "/custom/pmd"
    )


def test_resolve_pmd_executable_prefers_a_project_local_binary(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    _configure_pmd(repo)
    # Even when something named "pmd" is also on PATH, the project-local binary
    # under node_modules/.bin wins -- the same precedence jscpd resolution uses.
    monkeypatch.setattr(
        run_duplicate_audit.shutil, "which", lambda name: "/usr/bin/unrelated-pmd"
    )
    resolved = run_duplicate_audit._resolve_pmd_executable(repo)
    assert resolved == str((repo / "node_modules" / ".bin" / "pmd").resolve())


def test_resolve_pmd_executable_falls_back_to_path(tmp_path: Path, monkeypatch) -> None:
    repo = _init_repo(tmp_path)
    monkeypatch.setattr(
        run_duplicate_audit.shutil, "which", lambda name: "/usr/local/bin/pmd"
    )
    assert run_duplicate_audit._resolve_pmd_executable(repo) == "/usr/local/bin/pmd"


def test_resolve_pmd_executable_raises_when_nothing_resolves(
    tmp_path: Path, monkeypatch
) -> None:
    repo = _init_repo(tmp_path)
    monkeypatch.setattr(run_duplicate_audit.shutil, "which", lambda name: None)
    with pytest.raises(
        run_duplicate_audit.DuplicateAuditError, match="pmd executable is unavailable"
    ):
        run_duplicate_audit._resolve_pmd_executable(repo)


def test_pmd_index_check_reads_staged_baseline(tmp_path: Path, monkeypatch) -> None:
    repo = _init_repo(tmp_path)
    _configure_pmd(repo)
    first = repo / "src" / "first.py"
    second = repo / "native" / "second.py"
    fragment = "DUPLICATE_INDEX_SENTINEL = 1"
    first.write_text(fragment + "\n", encoding="utf-8")
    second.write_text(fragment + "\n", encoding="utf-8")
    baseline = repo / run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE
    _write_baseline(baseline)
    _git(
        repo,
        "add",
        "src/first.py",
        "native/second.py",
        run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE.as_posix(),
    )
    entry = {
        "key": _stable_dup_key("src/first.py", "native/second.py", fragment),
        "firstFile": "src/first.py",
        "secondFile": "native/second.py",
        "lines": 10,
    }
    _write_baseline(baseline, [entry])
    monkeypatch.setattr(run_duplicate_audit, "_run_report_command", _pmd_emulator)

    assert run_duplicate_audit.run_pmd_python(True, True, root=repo) == 1
    _git(repo, "add", run_duplicate_audit.PMD_CPD_BASELINE_RELATIVE.as_posix())
    assert run_duplicate_audit.run_pmd_python(True, True, root=repo) == 0
