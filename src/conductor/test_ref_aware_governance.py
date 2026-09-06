from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from conductor import check_duplicate_function_bodies
from conductor import check_protected_deletes
from conductor import guardrail_audit


def _git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    return proc.stdout.strip()


def _write(repo: Path, relative_path: str, content: str) -> None:
    path = repo / relative_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


@pytest.fixture
def governance_repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    _git(tmp_path, "init", "-b", "main")
    _git(tmp_path, "config", "user.email", "governance-tests@example.invalid")
    _git(tmp_path, "config", "user.name", "Governance Tests")
    _git(tmp_path, "config", "commit.gpgsign", "false")
    monkeypatch.setattr(guardrail_audit, "ROOT", tmp_path)
    monkeypatch.setattr(check_protected_deletes, "ROOT", tmp_path)
    monkeypatch.setattr(check_duplicate_function_bodies, "ROOT", tmp_path)
    # check_protected_deletes.main() now resolves its scan root from cwd's Git
    # toplevel (conductor/audit_root.py), not from the ROOT monkeypatch above,
    # so tests that drive it through main() need cwd inside this fake repo.
    monkeypatch.chdir(tmp_path)
    return tmp_path


def _commit_all(repo: Path, message: str) -> str:
    _git(repo, "add", "--all")
    _git(repo, "commit", "-m", message)
    return _git(repo, "rev-parse", "HEAD")


def _oversized_function(name: str) -> str:
    assignments = "\n".join(f"    value = {number}" for number in range(105))
    return f"def {name}():\n{assignments}\n    return value\n"


def _copyable_function(name: str) -> str:
    return (
        f"def {name}(value):\n"
        "    total = value + 1\n"
        "    total *= 2\n"
        "    total -= 3\n"
        "    total //= 4\n"
        "    total += 5\n"
        "    total *= 6\n"
        "    return total\n"
    )


def test_guardrail_audit_reads_staged_snapshot(
    governance_repo: Path,
) -> None:
    _write(governance_repo, "research/candidate.py", "value = 1\n")
    _commit_all(governance_repo, "base")
    _write(
        governance_repo,
        "research/candidate.py",
        _oversized_function("staged_candidate"),
    )
    _git(governance_repo, "add", "research/candidate.py")
    _write(governance_repo, "research/candidate.py", "value = 2\n")

    issues, summary = guardrail_audit.collect_issues(("research",), staged_only=True)

    assert summary["files_scanned"] == 1
    assert {issue.kind for issue in issues} == {"god_function"}


def test_guardrail_audit_reads_head_for_from_ref_with_clean_index(
    governance_repo: Path,
) -> None:
    _write(governance_repo, "research/candidate.py", "value = 1\n")
    base = _commit_all(governance_repo, "base")
    _write(
        governance_repo,
        "research/candidate.py",
        _oversized_function("committed_candidate"),
    )
    _commit_all(governance_repo, "candidate")
    _write(governance_repo, "research/candidate.py", "value = 2\n")
    assert _git(governance_repo, "diff", "--cached", "--name-only") == ""

    issues, summary = guardrail_audit.collect_issues(("research",), from_ref=base)

    assert summary["files_scanned"] == 1
    assert {issue.kind for issue in issues} == {"god_function"}


def test_protected_delete_reads_staged_index(governance_repo: Path) -> None:
    protected = "research/runtime/champion_example.json"
    _write(governance_repo, protected, "{}\n")
    _commit_all(governance_repo, "base")
    (governance_repo / protected).unlink()
    _git(governance_repo, "add", "--update")

    assert check_protected_deletes._deleted_paths() == [protected]
    assert check_protected_deletes.main([]) == 1


def test_protected_delete_reads_from_ref_with_clean_index(
    governance_repo: Path,
) -> None:
    protected = "research/runtime/champion_example.json"
    _write(governance_repo, protected, "{}\n")
    base = _commit_all(governance_repo, "base")
    (governance_repo / protected).unlink()
    _commit_all(governance_repo, "delete protected file")
    assert _git(governance_repo, "diff", "--cached", "--name-only") == ""

    assert check_protected_deletes._deleted_paths(base) == [protected]
    assert check_protected_deletes.main(["--from-ref", base]) == 1


def test_duplicate_body_reads_staged_index(governance_repo: Path) -> None:
    _write(
        governance_repo,
        "research/original.py",
        _copyable_function("original"),
    )
    _commit_all(governance_repo, "base")
    _write(
        governance_repo,
        "research/copied.py",
        _copyable_function("copied"),
    )
    _git(governance_repo, "add", "research/copied.py")
    _write(governance_repo, "research/copied.py", "value = 1\n")

    pairs = check_duplicate_function_bodies._duplicate_pairs()

    assert [(new.path, old.path) for new, old in pairs] == [
        ("research/copied.py", "research/original.py")
    ]


def test_duplicate_body_reads_from_ref_with_clean_index(
    governance_repo: Path,
) -> None:
    _write(
        governance_repo,
        "research/original.py",
        _copyable_function("original"),
    )
    base = _commit_all(governance_repo, "base")
    _write(
        governance_repo,
        "research/copied.py",
        _copyable_function("copied"),
    )
    _commit_all(governance_repo, "copy body")
    assert _git(governance_repo, "diff", "--cached", "--name-only") == ""

    pairs = check_duplicate_function_bodies._duplicate_pairs(base)

    assert [(new.path, old.path) for new, old in pairs] == [
        ("research/copied.py", "research/original.py")
    ]


def test_duplicate_body_from_ref_allows_move(governance_repo: Path) -> None:
    source = "research/original.py"
    destination = "research/package/moved.py"
    _write(governance_repo, source, _copyable_function("original"))
    base = _commit_all(governance_repo, "base")
    (governance_repo / source).unlink()
    _write(governance_repo, destination, _copyable_function("moved"))
    _commit_all(governance_repo, "move body")

    assert check_duplicate_function_bodies._duplicate_pairs(base) == []


def test_duplicate_body_git_and_cli_fail_closed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    failure = subprocess.CompletedProcess([], 2, stdout=b"", stderr=b"git failed")
    monkeypatch.setattr(check_duplicate_function_bodies, "_git", lambda _args: failure)
    with pytest.raises(RuntimeError, match="ls-tree failed"):
        check_duplicate_function_bodies._tracked_python_files("HEAD")
    with pytest.raises(RuntimeError, match="git diff failed"):
        check_duplicate_function_bodies._changed_python_files()
    with pytest.raises(RuntimeError, match="merge base"):
        check_duplicate_function_bodies._merge_base("HEAD^")

    monkeypatch.setattr(
        check_duplicate_function_bodies,
        "_changed_python_files",
        lambda _base_ref=None: [],
    )
    monkeypatch.setattr(
        check_duplicate_function_bodies, "_duplicate_pairs", lambda _ref=None: []
    )
    assert check_duplicate_function_bodies.main([]) == 0

    new = check_duplicate_function_bodies.FunctionBody("new.py", "new", 1, "a")
    old = check_duplicate_function_bodies.FunctionBody("old.py", "old", 2, "a")
    monkeypatch.setattr(
        check_duplicate_function_bodies,
        "_duplicate_pairs",
        lambda _ref=None: [(new, old)],
    )
    assert check_duplicate_function_bodies.main([]) == 1
