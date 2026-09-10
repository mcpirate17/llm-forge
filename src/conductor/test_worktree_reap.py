from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

from conductor import worktree_reap


def _porcelain(
    path: Path, head: str = "a" * 40, branch: str = "refs/heads/topic"
) -> str:
    return f"worktree {path}\nHEAD {head}\nbranch {branch}\n\n"


def test_parse_worktrees_preserves_safety_markers(tmp_path):
    rows = worktree_reap.parse_worktrees(
        "worktree "
        + str(tmp_path / "one")
        + "\nHEAD "
        + "a" * 40
        + "\nlocked reason\n\n"
        + "worktree /gone\nHEAD "
        + "b" * 40
        + "\nprunable gitdir missing\n\n"
    )
    assert rows[0].head == "a" * 40
    assert rows[0].locked
    assert rows[1].head == "b" * 40
    assert rows[1].prunable and rows[1].missing


def test_parse_worktrees_strips_head_branch_prefix_and_bare_marker(tmp_path):
    (tmp_path / "one").mkdir()
    rows = worktree_reap.parse_worktrees(
        f"worktree {tmp_path / 'one'}\n"
        "HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        "branch refs/heads/topic\n\n"
        "worktree /bare\n"
        "HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n"
        "bare\n\n"
    )
    assert rows[0].branch == "topic"
    assert not rows[0].missing
    assert rows[1].branch == ""
    assert rows[1].missing


def test_parse_worktrees_keeps_empty_head_and_boolean_marker_lines(tmp_path):
    path = tmp_path / "one"
    path.mkdir()
    rows = worktree_reap.parse_worktrees(
        f"worktree {path}\nbranch refs/heads/topic\nlocked\nprunable\nbare\n\n"
    )
    assert rows[0].head == ""
    assert rows[0].locked
    assert rows[0].prunable


def test_active_process_cwd_is_a_block(monkeypatch, tmp_path):
    target = tmp_path / "tree"
    target.mkdir()
    proc = tmp_path / "proc" / "42"
    proc.mkdir(parents=True)
    following_proc = tmp_path / "proc" / "43"
    following_proc.mkdir()
    final_proc = tmp_path / "proc" / "44"
    final_proc.mkdir()
    monkeypatch.setattr(
        worktree_reap.os,
        "readlink",
        lambda link: str(target / "nested") if link == proc / "cwd" else "",
    )
    assert worktree_reap.active_process_cwds(target, tmp_path / "proc") == [
        f"pid 42: {target / 'nested'}"
    ]
    for error in (PermissionError("denied"), RuntimeError("unreadable")):
        first_pid = next(proc.parent.iterdir()).name

        def fail_first_readlink(link):
            if link.parent.name == first_pid:
                raise error
            return str(target / link.parent.name)

        monkeypatch.setattr(worktree_reap.os, "readlink", fail_first_readlink)
        expected = sorted(
            [
                f"pid {entry.name}: {target / entry.name}"
                for entry in proc.parent.iterdir()
                if entry.name != first_pid
            ]
            + [f"unknown process state for pid {first_pid}: {error}"]
        )
        assert worktree_reap.active_process_cwds(target, tmp_path / "proc") == expected

    first_pid = next(proc.parent.iterdir()).name

    def vanished_first_readlink(link):
        if link.parent.name == first_pid:
            raise FileNotFoundError("exited")
        return str(target / link.parent.name)

    monkeypatch.setattr(worktree_reap.os, "readlink", vanished_first_readlink)
    expected = sorted(
        [
            f"pid {entry.name}: {target / entry.name}"
            for entry in proc.parent.iterdir()
            if entry.name != first_pid
        ]
    )
    assert worktree_reap.active_process_cwds(target, tmp_path / "proc") == expected


def test_status_of_a_non_git_directory_is_unknown_and_blocks(tmp_path):
    clean, reasons = worktree_reap._status(tmp_path)
    assert not clean
    assert reasons[0].startswith("unknown git status: fatal: not a git repository")


def test_status_requests_porcelain_and_all_untracked_files(monkeypatch, tmp_path):
    calls = []

    def fake_run(*args, **kwargs):
        calls.append((args, kwargs))
        return type("Done", (), {"returncode": 0, "stderr": "", "stdout": ""})()

    monkeypatch.setattr(worktree_reap.subprocess, "run", fake_run)
    assert worktree_reap._status(tmp_path) == (True, [])
    assert calls == [
        (
            (
                [
                    "git",
                    "-C",
                    str(tmp_path),
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                ],
            ),
            {"capture_output": True, "text": True, "check": False},
        )
    ]


def test_expired_lease_does_not_block_but_live_lease_does(tmp_path):
    path = tmp_path / "tree"
    path.mkdir()
    expiry = datetime.now(UTC) + timedelta(hours=1)
    (path / ".worktree-lease.json").write_text(
        '{"schema":"worktree-lease.v1","owner":"a","purpose":"b","expires_at":"'
        + expiry.isoformat()
        + '"}'
    )
    assert "active lease" in (
        worktree_reap._lease_reason(path, datetime.now(UTC)) or ""
    )
    assert worktree_reap._lease_reason(path, expiry + timedelta(seconds=1)) is None


def test_lease_with_naive_expiry_is_interpreted_as_utc(tmp_path):
    path = tmp_path / "tree"
    path.mkdir()
    (path / ".worktree-lease.json").write_text(
        '{"schema":"worktree-lease.v1","owner":"a","purpose":"b",'
        '"expires_at":"2099-01-01T00:00:00"}'
    )
    assert worktree_reap._lease_reason(path, datetime(2098, 1, 1, tzinfo=UTC))


def test_decide_refuses_dirty_locked_and_unproven(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40, locked=True),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(
        worktree_reap,
        "_status",
        lambda path: (True, ["?? saved.txt"]) if path == linked else (True, []),
    )
    decisions = worktree_reap.decide(
        tmp_path, current=tmp_path / "elsewhere", proc_root=tmp_path / "no-proc"
    )
    assert not decisions[0].eligible
    assert "primary worktree" in decisions[0].reasons
    assert not decisions[1].eligible
    assert any("dirty worktree" in reason for reason in decisions[1].reasons)
    assert "locked worktree" in decisions[1].reasons


def _decide_refuses_current_linked_tree_even_when_clean(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    proc_root = tmp_path / "proc"
    proc_root.mkdir()
    decisions = worktree_reap.decide(tmp_path, current=linked, proc_root=proc_root)
    assert not decisions[1].eligible
    assert decisions[1].reasons == ["current directory"]


def test_decide_refuses_current_nested_directory(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    nested = linked / "nested"
    nested.mkdir()
    proc_root = tmp_path / "proc"
    proc_root.mkdir()
    decisions = worktree_reap.decide(tmp_path, current=nested, proc_root=proc_root)
    assert decisions[1].reasons == ["current directory"]


def test_decide_refuses_unlinked_non_primary_tree(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40),
        ],
    )
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: False)
    decisions = worktree_reap.decide(
        tmp_path, current=tmp_path / "elsewhere", proc_root=tmp_path / "proc"
    )
    assert "primary worktree" in decisions[1].reasons


def test_decide_refuses_prunable_registration(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40, prunable=True),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    assert (
        "missing/prunable registration"
        in worktree_reap.decide(
            tmp_path, current=tmp_path / "elsewhere", proc_root=tmp_path / "proc"
        )[1].reasons
    )


def test_decide_default_clock_is_used_for_active_lease(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(
        worktree_reap,
        "_lease_reason",
        lambda path, now: (
            "active lease" if path == linked and now is not None else None
        ),
    )
    assert (
        "active lease"
        in worktree_reap.decide(
            tmp_path, current=tmp_path / "elsewhere", proc_root=tmp_path / "proc"
        )[1].reasons
    )


def test_decide_accepts_only_clean_ancestry_proof(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    linked = tmp_path / "linked"
    primary.mkdir()
    linked.mkdir()
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(linked, "b" * 40, branch="topic"),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path == linked
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    calls = []
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda repo, *args: (
            calls.append(args) or type("Done", (), {"returncode": 0, "stderr": ""})()
        ),
    )
    proc_root = tmp_path / "proc"
    proc_root.mkdir()
    decisions = worktree_reap.decide(
        tmp_path, current=tmp_path / "elsewhere", proc_root=proc_root
    )
    assert decisions[1].eligible
    assert decisions[1].reasons == ["ancestry proven in origin/master"]
    assert calls == [("merge-base", "--is-ancestor", "b" * 40, "origin/master")]
    monkeypatch.setattr(
        worktree_reap, "active_process_cwds", lambda *_: ["pid 9: process"]
    )
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: "active lease")
    guarded = worktree_reap.decide(
        tmp_path, current=tmp_path / "elsewhere", proc_root=proc_root
    )[1]
    assert not guarded.eligible
    assert guarded.reasons == ["pid 9: process", "active lease"]


def test_merged_pr_mode_selects_only_exact_branch_and_head(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    exact = tmp_path / "exact"
    other = tmp_path / "other"
    primary.mkdir()
    exact.mkdir()
    other.mkdir()
    exact_row = worktree_reap.Worktree(exact, "b" * 40, branch="topic")
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            exact_row,
            worktree_reap.Worktree(other, "c" * 40, branch="other"),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda path: path != primary
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(
        worktree_reap, "_merged_pr_proof", lambda *_: ("topic", "b" * 40)
    )
    proc_root = tmp_path / "proc"
    proc_root.mkdir()
    decisions = worktree_reap.decide(
        tmp_path, merged_pr=401, current=tmp_path / "elsewhere", proc_root=proc_root
    )
    assert decisions[1].eligible
    assert decisions[1].reasons == ["merged PR #401 exact branch/HEAD proven"]
    assert not decisions[2].eligible
    assert "does not match merged PR #401" in decisions[2].reasons[0]


def _merged_pr_proof_fails_closed_on_unknown_gh(monkeypatch, tmp_path):
    monkeypatch.setattr(
        worktree_reap.subprocess,
        "run",
        lambda *args, **kwargs: type(
            "Done", (), {"returncode": 1, "stderr": "not logged in", "stdout": ""}
        )(),
    )
    try:
        worktree_reap._merged_pr_proof(tmp_path, 401)
    except worktree_reap.ReapError as exc:
        assert "proof unavailable" in str(exc)
    else:
        raise AssertionError("unknown gh state was accepted")


def test_merged_pr_proof_uses_exact_gh_query_and_returns_head(monkeypatch, tmp_path):
    calls = []

    def fake_run(*args, **kwargs):
        calls.append((args, kwargs))
        return type(
            "Done",
            (),
            {
                "returncode": 0,
                "stderr": "",
                "stdout": '{"state":"MERGED","mergedAt":"2026-09-10T00:00:00Z",'
                '"headRefName":"topic","headRefOid":"b"}',
            },
        )()

    monkeypatch.setattr(worktree_reap.subprocess, "run", fake_run)
    assert worktree_reap._merged_pr_proof(tmp_path, 401) == ("topic", "b")
    assert calls[0][0][0] == [
        "gh",
        "pr",
        "view",
        "401",
        "--json",
        "state,mergedAt,headRefName,headRefOid",
    ]
    assert calls[0][1]["cwd"] == tmp_path


def test_merged_pr_proof_rejects_each_incomplete_merge_record(monkeypatch, tmp_path):
    records = [
        ([], "not proven MERGED"),
        ({"state": "OPEN", "mergedAt": "2026-09-10T00:00:00Z"}, "not proven MERGED"),
        ({"state": "MERGED"}, "not proven MERGED"),
        (
            {
                "state": "MERGED",
                "mergedAt": "2026-09-10T00:00:00Z",
                "headRefName": "",
                "headRefOid": "b",
            },
            "incomplete exact-head proof",
        ),
        (
            {
                "state": "MERGED",
                "mergedAt": "2026-09-10T00:00:00Z",
                "headRefName": "topic",
            },
            "incomplete exact-head proof",
        ),
        (
            {
                "state": "MERGED",
                "mergedAt": "2026-09-10T00:00:00Z",
                "headRefName": "topic",
                "headRefOid": "",
            },
            "incomplete exact-head proof",
        ),
    ]

    for record, expected in records:
        monkeypatch.setattr(
            worktree_reap.subprocess,
            "run",
            lambda *args, payload=record, **kwargs: type(
                "Done",
                (),
                {
                    "returncode": 0,
                    "stderr": "",
                    "stdout": json.dumps(payload),
                },
            )(),
        )
        try:
            worktree_reap._merged_pr_proof(tmp_path, 401)
        except worktree_reap.ReapError as exc:
            assert expected in str(exc)
        else:
            raise AssertionError(f"accepted incomplete merged PR record: {record!r}")
    _merged_pr_proof_fails_closed_on_unknown_gh(monkeypatch, tmp_path)


def test_apply_merged_pr_rechecks_exact_proof_without_ancestry(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["merged PR #401 exact branch/HEAD proven"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    monkeypatch.setattr(
        worktree_reap, "_merged_pr_proof", lambda *_: ("topic", "b" * 40)
    )
    calls = []
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda repo, *args: (
            calls.append(args) or type("Done", (), {"returncode": 0, "stderr": ""})()
        ),
    )
    assert worktree_reap.apply(tmp_path, [decision], merged_pr=401) == [str(path)]
    assert not any(args[:1] == ("merge-base",) for args in calls)


def test_apply_is_the_only_mutating_path(monkeypatch, tmp_path):
    path = tmp_path / "linked"
    (tmp_path / "primary").mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    calls: list[tuple[str, ...]] = []
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(tmp_path / "primary", "a" * 40),
            decision.worktree,
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda repo, *args: (
            calls.append(args) or type("Done", (), {"returncode": 0, "stderr": ""})()
        ),
    )
    assert worktree_reap.apply(tmp_path, [decision], delete_branches=True) == [
        str(path)
    ]
    assert calls == [
        ("merge-base", "--is-ancestor", "b" * 40, "origin/master"),
        ("worktree", "remove", str(path)),
        ("branch", "-d", "topic"),
    ]


def _apply_skips_ineligible_decisions(monkeypatch, tmp_path):
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(tmp_path / "linked"), False, ["dirty worktree"]
    )
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda *_: (_ for _ in ()).throw(AssertionError("must not mutate")),
    )
    assert worktree_reap.apply(tmp_path, [decision]) == []


def test_apply_continues_after_ineligible_decision(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decisions = [
        worktree_reap.Decision(
            worktree_reap.Worktree(tmp_path / "blocked"), False, ["dirty worktree"]
        ),
        worktree_reap.Decision(
            worktree_reap.Worktree(path, "b" * 40, branch="topic"),
            True,
            ["ancestry proven in origin/master"],
        ),
    ]
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decisions[1].worktree],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda repo, *args: type("Done", (), {"returncode": 0, "stderr": ""})(),
    )
    assert worktree_reap.apply(tmp_path, decisions) == [str(path)]
    for index, helper in enumerate(
        (
            _apply_skips_ineligible_decisions,
            _apply_rechecks_current_process_and_lease_guards,
            _apply_rechecks_primary_lock_and_unknown_status,
        )
    ):
        merged = tmp_path / f"merged-{index}"
        merged.mkdir()
        helper(monkeypatch, merged)


def test_apply_does_not_delete_branch_without_explicit_flag(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    calls = []
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda repo, *args: (
            calls.append(args) or type("Done", (), {"returncode": 0, "stderr": ""})()
        ),
    )
    assert worktree_reap.apply(tmp_path, [decision], delete_branches=False) == [
        str(path)
    ]
    assert not any(args[:2] == ("branch", "-d") for args in calls)
    for index, helper in enumerate(
        (
            _apply_rechecks_linked_identity_before_removal,
            _apply_rechecks_ancestry_proof_before_removal,
        )
    ):
        merged = tmp_path / f"merged-{index}"
        merged.mkdir()
        helper(monkeypatch, merged)


def test_main_rejects_branch_deletion_without_apply(tmp_path, capsys):
    try:
        worktree_reap.main(["--repo", str(tmp_path), "--delete-branches"])
    except SystemExit as exc:
        assert exc.code == 2
        assert "--delete-branches requires --apply" in capsys.readouterr().err
    else:
        raise AssertionError("branch deletion flag bypassed --apply requirement")


def test_apply_rechecks_head_before_removal(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(path, "c" * 40, branch="topic"),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda *_: type("Done", (), {"returncode": 0, "stderr": ""})(),
    )
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "HEAD/branch changed" in str(exc)
    else:
        raise AssertionError("apply trusted a stale HEAD")


def _apply_rechecks_primary_lock_and_unknown_status(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [
            worktree_reap.Worktree(primary, "a" * 40),
            worktree_reap.Worktree(path, "b" * 40, branch="topic", locked=True),
        ],
    )
    monkeypatch.setattr(
        worktree_reap, "is_linked_worktree", lambda candidate: candidate == path
    )
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "locked/prunable/missing" in str(exc)
    else:
        raise AssertionError("apply ignored a newly locked worktree")

    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(
        worktree_reap, "_status", lambda _: (False, ["unknown git status"])
    )
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "dirty/unknown" in str(exc)
    else:
        raise AssertionError("apply ignored unknown git status")


def _apply_rechecks_linked_identity_before_removal(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: False)
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "primary/non-linked" in str(exc)
    else:
        raise AssertionError("apply removed a no-longer-linked worktree")


def _apply_rechecks_current_process_and_lease_guards(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: True)
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(
        worktree_reap, "active_process_cwds", lambda *_: ["pid 7: process"]
    )
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "active-process" in str(exc)
    else:
        raise AssertionError("apply ignored a newly active process")

    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: "active lease")
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "active lease" in str(exc)
    else:
        raise AssertionError("apply ignored a newly active lease")


def _apply_rechecks_ancestry_proof_before_removal(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(path, "b" * 40, branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(
        worktree_reap,
        "inventory",
        lambda _: [worktree_reap.Worktree(primary, "a" * 40), decision.worktree],
    )
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: True)
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda *_: type("Done", (), {"returncode": 1, "stderr": "not ancestor"})(),
    )
    try:
        worktree_reap.apply(tmp_path, [decision])
    except worktree_reap.ReapError as exc:
        assert "unproven worktree" in str(exc)
    else:
        raise AssertionError("apply ignored failed ancestry proof")


def _recheck_rejects_nonlinked_before_downstream_guards(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    row = worktree_reap.Worktree(path, "b" * 40)
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: False)
    monkeypatch.setattr(
        worktree_reap,
        "_status",
        lambda _: (_ for _ in ()).throw(AssertionError("status reached")),
    )
    try:
        worktree_reap._recheck_tree_state(
            tmp_path, row, [worktree_reap.Worktree(primary), row], current=tmp_path
        )
    except worktree_reap.ReapError as exc:
        assert "primary/non-linked" in str(exc)
    else:
        raise AssertionError("non-linked worktree was not rejected")


def test_recheck_rejects_lock_prune_or_missing_before_status(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: True)
    monkeypatch.setattr(
        worktree_reap,
        "_status",
        lambda _: (_ for _ in ()).throw(AssertionError("status reached")),
    )
    primary_row = worktree_reap.Worktree(primary, "a" * 40)
    try:
        worktree_reap._recheck_tree_state(
            tmp_path, primary_row, [primary_row], current=tmp_path / "elsewhere"
        )
    except worktree_reap.ReapError as exc:
        assert "primary/non-linked" in str(exc)
    else:
        raise AssertionError("primary worktree was not rejected")
    for row in (
        worktree_reap.Worktree(path, "b" * 40, locked=True),
        worktree_reap.Worktree(path, "b" * 40, prunable=True),
        worktree_reap.Worktree(tmp_path / "gone", "b" * 40),
    ):
        try:
            worktree_reap._recheck_tree_state(
                tmp_path,
                row,
                [worktree_reap.Worktree(primary), row],
                current=tmp_path / "elsewhere",
            )
        except worktree_reap.ReapError as exc:
            assert "locked/prunable/missing" in str(exc)
        else:
            raise AssertionError("unsafe worktree marker was not rejected")
    merged = tmp_path / "merged"
    merged.mkdir()
    _recheck_rejects_nonlinked_before_downstream_guards(monkeypatch, merged)


def test_recheck_rejects_unknown_or_dirty_before_process_probe(monkeypatch, tmp_path):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    primary.mkdir()
    path.mkdir()
    row = worktree_reap.Worktree(path, "b" * 40)
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: True)
    monkeypatch.setattr(
        worktree_reap,
        "active_process_cwds",
        lambda *_: (_ for _ in ()).throw(AssertionError("process reached")),
    )
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    for status in (
        (False, ["unknown"]),
        (True, ["?? dirty"]),
    ):
        monkeypatch.setattr(worktree_reap, "_status", lambda _: status)
        try:
            worktree_reap._recheck_tree_state(
                tmp_path,
                row,
                [worktree_reap.Worktree(primary), row],
                current=tmp_path / "elsewhere",
            )
        except worktree_reap.ReapError as exc:
            assert "dirty/unknown" in str(exc)
        else:
            raise AssertionError("unsafe status was not rejected")


def test_recheck_rejects_current_equal_or_nested_before_process_probe(
    monkeypatch, tmp_path
):
    primary = tmp_path / "primary"
    path = tmp_path / "linked"
    nested = path / "nested"
    primary.mkdir()
    path.mkdir()
    nested.mkdir()
    row = worktree_reap.Worktree(path, "b" * 40)
    monkeypatch.setattr(worktree_reap, "is_linked_worktree", lambda _: True)
    monkeypatch.setattr(worktree_reap, "_status", lambda _: (True, []))
    monkeypatch.setattr(
        worktree_reap,
        "active_process_cwds",
        lambda *_: (_ for _ in ()).throw(AssertionError("process reached")),
    )
    monkeypatch.setattr(worktree_reap, "_lease_reason", lambda *_: None)
    for current in (path, nested):
        try:
            worktree_reap._recheck_tree_state(
                tmp_path, row, [worktree_reap.Worktree(primary), row], current=current
            )
        except worktree_reap.ReapError as exc:
            assert "current worktree" in str(exc)
        else:
            raise AssertionError("current worktree was not rejected")
    monkeypatch.setattr(worktree_reap, "active_process_cwds", lambda *_: [])
    merged = tmp_path / "merged"
    merged.mkdir()
    _decide_refuses_current_linked_tree_even_when_clean(monkeypatch, merged)


def test_inventory_surfaces_git_failure(monkeypatch, tmp_path):
    monkeypatch.setattr(
        worktree_reap,
        "_run",
        lambda *_: type("Done", (), {"returncode": 1, "stderr": "broken"})(),
    )
    try:
        worktree_reap.inventory(tmp_path)
    except worktree_reap.ReapError as exc:
        assert "git worktree list --porcelain failed: broken" in str(exc)
    else:
        raise AssertionError("inventory accepted a failed git command")


def test_main_defaults_to_report_only_and_json(monkeypatch, tmp_path, capsys):
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(tmp_path / "linked", branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(worktree_reap, "decide", lambda *args, **kwargs: [decision])
    monkeypatch.setattr(
        worktree_reap,
        "apply",
        lambda *args, **kwargs: (_ for _ in ()).throw(
            AssertionError("dry-run mutated")
        ),
    )
    assert worktree_reap.main(["--repo", str(tmp_path), "--json"]) == 0
    output = capsys.readouterr().out
    assert '"dry_run": true' in output
    assert '"decisions"' in output
    assert '"removed": []' in output
    _main_apply_reports_removed(monkeypatch, tmp_path, capsys)


def _main_apply_reports_removed(monkeypatch, tmp_path, capsys):
    decision = worktree_reap.Decision(
        worktree_reap.Worktree(tmp_path / "linked", branch="topic"),
        True,
        ["ancestry proven in origin/master"],
    )
    monkeypatch.setattr(worktree_reap, "decide", lambda *args, **kwargs: [decision])
    monkeypatch.setattr(
        worktree_reap, "apply", lambda *args, **kwargs: [str(decision.worktree.path)]
    )
    assert worktree_reap.main(["--repo", str(tmp_path), "--apply", "--json"]) == 0
    assert '"dry_run": false' in capsys.readouterr().out


def test_main_text_bounds_reasons_but_json_preserves_them(
    monkeypatch, tmp_path, capsys
):
    for count in (0, 5, 6, 8):
        reasons = [f"reason-{index}" for index in range(count)]
        decision = worktree_reap.Decision(
            worktree_reap.Worktree(tmp_path / "linked", branch="topic"),
            False,
            reasons,
        )
        monkeypatch.setattr(worktree_reap, "decide", lambda *args, **kwargs: [decision])
        assert worktree_reap.main(["--repo", str(tmp_path)]) == 0
        output = capsys.readouterr().out
        expected = "; ".join(reasons[:5])
        if count > 5:
            expected += f"; {count - 5} more reasons (use --json)"
        assert (
            output.splitlines()[0]
            == f"KEEP {tmp_path / 'linked'} [topic] -- {expected}"
        )
        assert output.splitlines()[1:] == [
            "dry-run only; pass --apply to remove eligible worktrees"
        ]
        assert decision.reasons == reasons
        assert worktree_reap.main(["--repo", str(tmp_path), "--json"]) == 0
        import json

        payload = json.loads(capsys.readouterr().out)
        assert payload["decisions"][0]["reasons"] == reasons
        decision.eligible = True
        monkeypatch.setattr(worktree_reap, "apply", lambda *args, **kwargs: [])
        assert worktree_reap.main(["--repo", str(tmp_path), "--apply"]) == 0
        assert capsys.readouterr().out.splitlines() == [
            f"REMOVE {tmp_path / 'linked'} [topic] -- {expected}"
        ]
