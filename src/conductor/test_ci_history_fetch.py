"""The ci_history fetcher: recorded gh JSON, real local git, no network."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from conductor import ci_history_fetch as fetcher
from conductor.ci_history_fetch import (
    CiHistory,
    CiTrailers,
    GhClient,
    first_push_ci,
    main,
    trailers_of,
)

# The Rust fixture (`native/forge/src/ledger/outcome.rs`'s FIXTURE_JSON),
# copied verbatim: the shape this module writes is frozen by what the
# reader accepts, so the copy is the contract.
RUST_FIXTURE = {
    "fetched_utc": "2026-09-13T00:00:00Z",
    "prs": {
        "50": {
            "branch": "forge/routing-policy",
            "first_push_sha": "aaa111",
            "first_push_ci": "red",
            "commits": [
                {
                    "sha": "aaa111",
                    "subject": "feat: first cut",
                    "trailers": {"Agent": "glm"},
                },
                {
                    "sha": "bbb222",
                    "subject": "fix: CI",
                    "trailers": {
                        "Agent": "llm-b0",
                        "Claude-Session": "https://claude.ai/code/session_01X",
                    },
                },
            ],
        },
        "48": {
            "branch": "forge/ledger-join",
            "first_push_sha": "ccc333",
            "first_push_ci": "green",
            "commits": [
                {
                    "sha": "ccc333",
                    "subject": "feat: clean landing",
                    "trailers": {"Agent": "llm-b0"},
                }
            ],
        },
    },
}

# Recorded gh reply shapes: `pr list --state merged --json
# number,headRefName,mergedAt`, `pr view --json number,headRefName,commits`
# (one commit per entry: oid + messageHeadline is all the fetcher reads),
# and `api commits/{sha}/check-runs` (status + conclusion per run).
RECORDED_LIST = [
    {"number": 50, "headRefName": "forge/routing-policy",
     "mergedAt": "2026-09-12T10:00:00Z"},
    {"number": 51, "headRefName": "forge/bytecode",
     "mergedAt": "2026-09-12T11:00:00Z"},
]
CHECK_RUNS_RED = {
    "total_count": 2,
    "check_runs": [
        {"status": "completed", "conclusion": "success"},
        {"status": "completed", "conclusion": "failure"},
    ],
}
CHECK_RUNS_GREEN = {
    "total_count": 2,
    "check_runs": [
        {"status": "completed", "conclusion": "success"},
        {"status": "completed", "conclusion": "skipped"},
    ],
}
CHECK_RUNS_UNKNOWN = {
    "total_count": 2,
    "check_runs": [
        {"status": "in_progress", "conclusion": None},
        {"status": "queued", "conclusion": None},
    ],
}


class FakeGh(GhClient):
    """A GhClient with recorded replies and a tally of every call made."""

    def __init__(self, replies: dict[str, Any], lists: list[dict[str, Any]]) -> None:
        super().__init__(Path("."), "octo", "widget")
        self.replies = replies
        self.lists = lists
        self.calls: list[str] = []

    def require_ready(self) -> None:
        self.calls.append("auth")

    def list_merged(self, *, limit: int | None = None) -> list[dict[str, Any]]:
        self.calls.append("list")
        return self.lists[: (limit or len(self.lists))]

    def view_pr(self, number: int) -> dict[str, Any]:
        self.calls.append(f"view:{number}")
        return self.replies[f"view:{number}"]

    def check_runs(self, sha: str) -> dict[str, Any]:
        self.calls.append(f"checks:{sha}")
        return self.replies[f"checks:{sha}"]


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert completed.returncode == 0, completed.stderr
    return completed.stdout


def _pr_fixture_repo(tmp_path: Path) -> tuple[Path, list[str]]:
    """A local clone whose origin holds two PR heads the clone itself lacks.

    Commits are made in a work repo and pushed to a bare origin under
    ``refs/pull/50/head`` (two commits) and ``refs/pull/51/head`` (one) --
    the permanent refs the fetcher fetches -- and the clone is made fresh
    from the branchless bare so none of the PR commits are present locally
    until ``commit_messages`` fetches them, which is the exact situation a
    squash-merged PR leaves behind.
    """

    origin = tmp_path / "origin.git"
    _git(tmp_path, "init", "-q", "--bare", str(origin))
    work = tmp_path / "work"
    work.mkdir()
    _git(work, "init", "-q", "-b", "main")
    _git(work, "config", "user.name", "t")
    _git(work, "config", "user.email", "t@t")
    shas = []
    for message, pr in (
        ("feat: first cut\n\nLong body.\n\nAgent: glm\n", None),
        (
            "fix: CI\n\nAgent: llm-b0\n"
            "Claude-Session: https://claude.ai/code/session_01X\n"
            "Co-Authored-By: Someone <s@example.com>\n",
            "50",
        ),
        ("feat: newer thing\n\nAgent: glm\n", "51"),
    ):
        (work / "f.txt").write_text(message, encoding="utf-8")
        _git(work, "add", "-A")
        _git(work, "commit", "-q", "-m", message)
        shas.append(_git(work, "rev-parse", "HEAD").strip())
        if pr is not None:
            _git(work, "push", "-q", str(origin), f"HEAD:refs/pull/{pr}/head")
    clone = tmp_path / "clone"
    _git(tmp_path, "clone", "-q", "--origin", "origin", str(origin), str(clone))
    _git(clone, "config", "user.name", "t")
    _git(clone, "config", "user.email", "t@t")
    return clone, shas


def _view_50(shas: list[str]) -> dict[str, Any]:
    return {
        "number": 50,
        "headRefName": "forge/routing-policy",
        "commits": [
            {"oid": shas[0], "messageHeadline": "feat: first cut"},
            {"oid": shas[1], "messageHeadline": "fix: CI"},
        ],
    }


def _github_origin(clone: Path) -> None:
    """Point the clone's origin at a github.com URL, as main() parses it.

    The fetch-path tests keep origin on the local bare (they really fetch
    PR heads); only main()'s slug parsing needs the github.com shape, and
    it never touches the network before the code under test raises.
    """

    _git(clone, "remote", "set-url", "origin", "https://github.com/octo/widget.git")


def test_the_schema_round_trips_the_rust_fixture() -> None:
    """What this module writes is what the frozen Rust fixture reads.

    The fixture loads through the Pydantic model and dumps back to the
    same shape: identical key sets per level, `prs` keyed by PR number
    string, trailers absent when absent (never empty strings).
    """

    history = CiHistory.model_validate(RUST_FIXTURE)
    dumped = json.loads(history.dump())

    assert set(dumped) == {"fetched_utc", "prs"} == set(RUST_FIXTURE)
    assert set(dumped["prs"]) == set(RUST_FIXTURE["prs"])
    for number, row in RUST_FIXTURE["prs"].items():
        assert set(dumped["prs"][number]) == set(row) == {
            "branch",
            "first_push_sha",
            "first_push_ci",
            "commits",
        }
        for index, commit in enumerate(row["commits"]):
            written = dumped["prs"][number]["commits"][index]
            assert set(written) == set(commit)
            assert set(written["trailers"]) == set(commit["trailers"])
    # Absent trailers are absent keys on write, never "".
    bare = CiTrailers.model_validate({})
    assert "Agent" not in bare.model_dump(by_alias=True, exclude_none=True)
    assert "Agent" in CiTrailers.model_validate(
        {"Agent": "glm"}
    ).model_dump(by_alias=True, exclude_none=True)


def test_first_push_ci_classifies_the_recorded_check_runs() -> None:
    """green iff every completed run succeeded, red iff any failed, else unknown."""

    assert first_push_ci(CHECK_RUNS_GREEN) == "green"
    assert first_push_ci(CHECK_RUNS_RED) == "red"
    assert first_push_ci(CHECK_RUNS_UNKNOWN) == "unknown"
    assert first_push_ci({"check_runs": []}) == "unknown"


def test_trailers_come_from_interpret_trailers_not_a_regex(
    tmp_path: Path,
) -> None:
    """The tool owns what a trailer is: folding, not matching."""

    message = (
        "feat: a subject\n"
        "\n"
        "Body line.\n"
        "\n"
        "Agent: glm\n"
        "Claude-Session: https://claude.ai/code/session_1\n"
        "Signed-off-by: A U Thor <author@example.com>\n"
    )
    trailers = trailers_of(message, tmp_path)
    assert trailers["Agent"] == "glm"
    assert trailers["Claude-Session"] == "https://claude.ai/code/session_1"
    assert trailers_of("no trailers here\n", tmp_path) == {}


def test_a_full_fetch_reads_the_pr_head_and_writes_the_cache(
    tmp_path: Path,
) -> None:
    """End to end against recorded gh JSON and a real local git clone."""

    clone, shas = _pr_fixture_repo(tmp_path)
    gh = FakeGh(
        {
            "view:50": _view_50(shas),
            f"checks:{shas[0]}": CHECK_RUNS_RED,
        },
        RECORDED_LIST[:1],
    )
    out = tmp_path / "cache.json"

    code, unresolved = fetcher.fetch(gh, clone, out)

    assert (code, unresolved) == (0, [])
    written = json.loads(out.read_text(encoding="utf-8"))
    (row,) = written["prs"].values()
    assert row["branch"] == "forge/routing-policy"
    assert row["first_push_sha"] == shas[0]
    assert row["first_push_ci"] == "red"
    assert [c["sha"] for c in row["commits"]] == [shas[0], shas[1]]
    # Trailers were parsed from the real commit messages fetched from the
    # PR head; subjects came from gh.
    assert row["commits"][0]["trailers"] == {"Agent": "glm"}
    assert row["commits"][1]["trailers"] == {
        "Agent": "llm-b0",
        "Claude-Session": "https://claude.ai/code/session_01X",
    }
    assert row["commits"][0]["subject"] == "feat: first cut"
    # The atomic write leaves no temp behind.
    assert not list(out.parent.glob(f".{out.name}.tmp"))


def test_an_incremental_run_leaves_cached_prs_alone(tmp_path: Path) -> None:
    """A PR merged before the last fetch is not fetched again, verbatim."""

    clone, shas = _pr_fixture_repo(tmp_path)
    out = tmp_path / "cache.json"
    out.write_text(json.dumps(RUST_FIXTURE), encoding="utf-8")
    # PR 48 is cached and merged 2026-09-12, before fetched_utc
    # 2026-09-13; PR 51 is new.
    newer = {
        "number": 51,
        "headRefName": "forge/bytecode",
        "commits": [{"oid": shas[2], "messageHeadline": "feat: newer thing"}],
    }
    gh = FakeGh(
        {
            "view:51": newer,
            f"checks:{shas[2]}": CHECK_RUNS_GREEN,
        },
        [
            {"number": 48, "headRefName": "old", "mergedAt": "2026-09-12T09:00:00Z"},
            {"number": 51, "headRefName": "forge/bytecode",
             "mergedAt": "2026-09-13T12:00:00Z"},
        ],
    )

    code, unresolved = fetcher.fetch(gh, clone, out)

    assert (code, unresolved) == (0, [])
    assert "view:48" not in gh.calls, "a cached unchanged PR must not be re-fetched"
    written = json.loads(out.read_text(encoding="utf-8"))
    assert written["prs"]["48"] == RUST_FIXTURE["prs"]["48"]
    assert written["prs"]["51"]["first_push_ci"] == "green"


def test_an_unresolvable_pr_keeps_every_resolvable_one(tmp_path: Path) -> None:
    """A PR that cannot be resolved is named, the rest still written, exit 1."""

    clone, shas = _pr_fixture_repo(tmp_path)

    class HalfBroken(FakeGh):
        def view_pr(self, number: int) -> dict[str, Any]:
            if number == 50:
                raise fetcher.GhError("boom")
            return super().view_pr(number)

    out = tmp_path / "cache.json"
    gh = HalfBroken(
        {
            "view:51": {
                "number": 51,
                "headRefName": "forge/bytecode",
                "commits": [{"oid": shas[2], "messageHeadline": "feat: newer thing"}],
            },
            f"checks:{shas[2]}": CHECK_RUNS_GREEN,
        },
        RECORDED_LIST,
    )

    code, unresolved = fetcher.fetch(gh, clone, out)

    assert (code, unresolved) == (1, [50])
    written = json.loads(out.read_text(encoding="utf-8"))
    assert set(written["prs"]) == {"51"}


def test_main_exits_2_without_gh_and_writes_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """A missing gh is the exact missing thing, and no partial file exists."""

    clone, _ = _pr_fixture_repo(tmp_path)
    _github_origin(clone)
    out = tmp_path / "nested" / "cache.json"

    def missing(*args: str) -> str:
        raise fetcher.EnvError("gh is not on PATH")

    monkeypatch.setattr(GhClient, "require_ready", missing)
    assert main(["--repo", str(clone), "--out", str(out)]) == 2
    assert "gh is not on PATH" in capsys.readouterr().err
    assert not out.exists() and not out.parent.exists()


def test_main_exits_2_on_an_unauthenticated_gh(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    clone, _ = _pr_fixture_repo(tmp_path)
    _github_origin(clone)

    def unauthenticated(*args: str) -> str:
        raise fetcher.EnvError(
            "gh is not authenticated: To get started with GitHub CLI, run gh auth login"
        )

    monkeypatch.setattr(GhClient, "require_ready", unauthenticated)
    rc = main(["--repo", str(clone), "--out", str(tmp_path / "cache.json")])
    assert rc == 2
    assert "gh auth login" in capsys.readouterr().err


def test_main_exits_2_on_an_unreadable_existing_cache(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """An existing file this fetcher cannot parse is not clobbered either."""

    clone, _ = _pr_fixture_repo(tmp_path)
    _github_origin(clone)
    monkeypatch.setattr(
        GhClient, "require_ready", lambda self: None
    )  # gh itself is not what this test is about
    out = tmp_path / "cache.json"
    out.write_text("{not json", encoding="utf-8")

    assert main(["--repo", str(clone), "--out", str(out)]) == 2
    assert "unreadable" in capsys.readouterr().err
    assert out.read_text(encoding="utf-8") == "{not json"


def test_dry_run_calls_no_gh_and_writes_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """--dry-run prints the plan; one gh call would fail the test."""

    clone, _ = _pr_fixture_repo(tmp_path)
    _github_origin(clone)
    out = tmp_path / "cache.json"
    out.write_text(json.dumps(RUST_FIXTURE), encoding="utf-8")

    def forbidden(self: GhClient, *args: str) -> str:
        raise AssertionError("dry-run must not call gh")

    monkeypatch.setattr(GhClient, "_run_gh", forbidden)
    monkeypatch.setattr(GhClient, "require_ready", forbidden)

    assert main(["--repo", str(clone), "--out", str(out), "--dry-run"]) == 0
    plan = json.loads(capsys.readouterr().out)
    assert plan["dry_run"] is True and plan["slug"] == "octo/widget"
    assert plan["cached_prs_left_alone"] == ["48", "50"]
    assert out.read_text(encoding="utf-8") == json.dumps(RUST_FIXTURE)
