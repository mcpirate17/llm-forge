"""Tests for the graph-gate claim check messages."""

from __future__ import annotations

import importlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

HOOK_DIR = Path(__file__).resolve().parent
ROOT = HOOK_DIR.parents[2]


@pytest.fixture
def gate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "--quiet"], cwd=repo, check=True)
    (repo / "a.py").write_text("A = 1\n", encoding="utf-8")
    (repo / "b.py").write_text("B = 1\n", encoding="utf-8")
    sys.path.insert(0, str(ROOT))
    from conductor.candidate_review.ownership import create_claim

    create_claim(
        repo, owner="codex-phase22", paths=["a.py"], justification="j", hours=1
    )
    create_claim(repo, owner="claude", paths=["b.py"], justification="j", hours=1)
    monkeypatch.setenv("CRG_GATE_REPO_ROOT", str(repo))
    sys.path.insert(0, str(HOOK_DIR))
    import crg_gate

    return importlib.reload(crg_gate)


def test_owner_with_live_claim_is_allowed(gate) -> None:
    allowed, detail = gate._claim_allows("claude", "b.py")
    assert allowed and len(detail) == 64  # store digest


def test_denial_names_the_holder_and_expiry(gate) -> None:
    allowed, detail = gate._claim_allows("claude", "a.py")
    assert not allowed
    assert detail.startswith("path 'a.py' is held by codex-phase22 until ")
    assert "(claim-" in detail and "owner='claude' has no live claim" in detail
    assert "coordinate via A2A" in detail


def test_denial_without_any_holder_keeps_plain_message(gate) -> None:
    allowed, detail = gate._claim_allows("claude", "c.py")
    assert not allowed
    assert detail == "no live exact claim for owner='claude' path='c.py'"
    assert gate._claim_allows("", "a.py") == (
        False,
        "hook has no GOVERNANCE_OWNER identity",
    )


@pytest.fixture
def bash_targets():
    sys.path.insert(0, str(HOOK_DIR))
    import bash_write_targets

    return importlib.reload(bash_write_targets)


def test_read_only_commands_have_no_write_targets(bash_targets) -> None:
    for command in ("git status --porcelain", "ls -la; cat README.md | head -20"):
        assert bash_targets.write_targets(command) == []


def test_descriptor_duplication_is_not_a_write(bash_targets) -> None:
    """`2>&1` opens no file; treating it as one denies most piped commands."""
    assert bash_targets.write_targets("make check 2>&1 | tail -5") == []
    assert bash_targets.write_targets("echo hi > /dev/null 2>&1") == []


def test_interpreter_heredoc_resolves_a_literal_target(bash_targets) -> None:
    """The heredoc body is program text, so its writes must be seen."""
    command = (
        "python3 - <<'EOF'\nimport pathlib\n"
        "pathlib.Path('CLAUDE.md').write_bytes(body)\nEOF"
    )
    assert bash_targets.write_targets(command) == ["CLAUDE.md"]


def test_write_content_is_not_mistaken_for_a_path(bash_targets) -> None:
    """`write_text` takes content; reading it as a path invents a phantom file."""
    command = (
        "python3 -c \"from pathlib import Path; Path('a.py').write_text('SECRET')\""
    )
    assert bash_targets.write_targets(command) == ["a.py"]


def test_unresolvable_interpreter_write_is_reported_opaque(bash_targets) -> None:
    command = (
        "python3 - <<'EOF'\nimport pathlib\npathlib.Path(target).write_text('x')\nEOF"
    )
    assert bash_targets.write_targets(command) == [bash_targets.OPAQUE_WRITE]


def test_data_heredoc_body_is_not_scanned_for_writes(bash_targets) -> None:
    """A `cat` heredoc body is data; scanning it invents targets from prose."""
    command = "cat <<'EOF'\nPath('x').write_text('y')\nEOF"
    assert bash_targets.write_targets(command) == []


def test_paths_outside_the_repo_are_dropped(bash_targets, tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    command = "git log > /tmp/x.log && echo y > inside.txt"
    assert bash_targets.repo_write_targets(command, repo) == ["inside.txt"]


def _bash_decision(gate, capsys, command: str) -> str:
    """Run verify_bash and return its permission decision ('allow' if silent)."""
    payload = {
        "session_id": "s1",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    }
    gate._write_state(
        gate._state_path(gate._state_dir(), gate._state_key(payload), "graph-used")
    )
    capsys.readouterr()
    assert gate.verify_bash(payload, owner="claude") == 0
    out = capsys.readouterr().out.strip()
    if not out:
        return "allow"
    return json.loads(out)["hookSpecificOutput"]["permissionDecisionReason"]


@pytest.fixture
def bash_gate(gate, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("CRG_GATE_STATE_DIR", str(tmp_path / "state"))
    return gate


def test_verify_bash_denies_a_write_to_another_owners_path(bash_gate, capsys) -> None:
    """`a.py` is held by codex-phase22 in the fixture repo."""
    reason = _bash_decision(bash_gate, capsys, "echo x >> a.py")
    assert reason.startswith("BLOCKED: path 'a.py' is held by codex-phase22")


def test_verify_bash_allows_a_write_to_a_claimed_path(bash_gate, capsys) -> None:
    """`b.py` is claimed by claude; the gate must not block its own owner."""
    assert _bash_decision(bash_gate, capsys, "sed -i s/B/C/ b.py") == "allow"


def test_verify_bash_allows_a_read_only_command(bash_gate, capsys) -> None:
    assert _bash_decision(bash_gate, capsys, "ls -la | head -20") == "allow"


def test_verify_bash_denies_an_unresolvable_write(bash_gate, capsys) -> None:
    """Uses an inline `-c` shape, so this pins the gate's response rather than
    duplicating a resolver test that already covers the same opaque source."""
    reason = _bash_decision(
        bash_gate, capsys, 'python3 -c "import shutil; shutil.rmtree(target)"'
    )
    assert "cannot be resolved" in reason and "Use Edit/Write" in reason


def test_verify_bash_requires_the_graph_call_before_a_write(
    gate, tmp_path, monkeypatch, capsys
) -> None:
    monkeypatch.setenv("CRG_GATE_STATE_DIR", str(tmp_path / "no-graph"))
    payload = {
        "session_id": "s2",
        "tool_name": "Bash",
        "tool_input": {"command": "echo x >> b.py"},
    }
    assert gate.verify_bash(payload, owner="claude") == 0
    reason = json.loads(capsys.readouterr().out)["hookSpecificOutput"][
        "permissionDecisionReason"
    ]
    assert "call a code-review-graph MCP tool" in reason and "b.py" in reason


def test_stream_merge_redirect_is_a_write(bash_targets) -> None:
    """`&> log` opens a file for both streams; missing it hides a real write."""
    assert bash_targets.write_targets("make check &> build.log") == ["build.log"]


def test_command_local_variable_is_expanded(bash_targets) -> None:
    """shlex does not expand `$VAR`; unexpanded, a /tmp write looks repo-local."""
    for form in ("$SP", "${SP}"):
        command = f'SP="/tmp/scratch"; echo hi > {form}/note.txt'
        assert bash_targets.write_targets(command) == ["/tmp/scratch/note.txt"]
        assert (
            bash_targets.repo_write_targets(command, Path("/home/tim/Projects/LLM"))
            == []
        )


def test_unresolvable_variable_is_reported_opaque(bash_targets) -> None:
    """An environment variable cannot be resolved here, so it is not guessed at."""
    assert bash_targets.write_targets("echo hi > $HOME/note.txt") == [
        bash_targets.OPAQUE_WRITE
    ]


def test_cd_outside_the_repo_moves_relative_targets(bash_targets) -> None:
    """`cd /tmp && ... > f` writes to /tmp, not to the repo root."""
    repo = Path("/home/tim/Projects/LLM")
    command = "cd /tmp/scratch && echo hi > note.txt"
    assert bash_targets.repo_write_targets(command, repo) == []


def test_cd_into_the_repo_resolves_against_that_subdirectory(bash_targets) -> None:
    repo = Path("/home/tim/Projects/LLM")
    command = "cd conductor && sed -i s/a/b/ handoff.py"
    assert bash_targets.repo_write_targets(command, repo) == ["conductor/handoff.py"]


def test_unresolvable_cd_makes_relative_targets_opaque(bash_targets) -> None:
    """An unset variable, a bare `cd` (goes to $HOME) and `cd -` (the previous
    directory) are all unknowable here, so relative writes after them cannot be
    resolved and must not be guessed at."""
    repo = Path("/home/tim/Projects/LLM")
    for command in (
        "cd $SOMEWHERE && echo hi > note.txt",
        "cd && echo hi > note.txt",
        "cd - && echo hi > note.txt",
    ):
        assert bash_targets.repo_write_targets(command, repo) == [
            bash_targets.OPAQUE_WRITE
        ]


def test_quoted_heredoc_mention_is_not_a_redirection(bash_targets) -> None:
    """A payload mentioning `<<EOF` must not swallow the rest of the command.

    Multi-line on purpose: a single-line mention is harmless because the scan runs
    out of lines immediately. It is the multi-line argument -- a `handoff append
    --body` describing a heredoc -- that made the scanner hunt for a terminator
    that never comes and discard every command after it.
    """
    command = (
        'python -m conductor.handoff append --title t --body "line one\n'
        "mentions python3 - <<EOF\n"
        'and continues" && echo done > out.txt'
    )
    assert bash_targets.write_targets(command) == ["out.txt"]


def test_commands_on_separate_lines_stay_separate(bash_targets) -> None:
    """shlex drops newlines, so two commands merged and the last token won."""
    command = "cp a.py b.py\ngit -C /tmp status --porcelain"
    assert bash_targets.write_targets(command) == ["b.py"]


def test_literal_loop_list_expands_to_every_target(bash_targets) -> None:
    command = 'for f in a.py b.py; do sed -i s/x/y/ "$f"; done'
    assert bash_targets.write_targets(command) == ["a.py", "b.py"]


def test_computed_loop_list_stays_opaque(bash_targets) -> None:
    """`for f in $(git ls-files)` cannot be resolved and must not be guessed."""
    command = 'for f in $(git ls-files); do rm "$f"; done'
    assert bash_targets.write_targets(command) == [bash_targets.OPAQUE_WRITE]


def test_loop_list_hoisted_into_a_variable_still_expands(bash_targets) -> None:
    """`FILES=...; for f in $FILES` is the same idiom with the list hoisted."""
    command = "FILES='a.py b.py'\nfor f in $FILES; do sed -i s/x/y/ \"$f\"; done"
    assert bash_targets.write_targets(command) == ["a.py", "b.py"]


def test_a_path_the_script_only_reads_is_not_a_target(bash_targets) -> None:
    """Collecting every `Path(...)` denied a script for "writing" the repo root
    when it merely named it. Only a written path counts."""
    source = (
        "python3 -c 'from pathlib import Path; "
        'root = Path("/home/tim/Projects/LLM"); '
        'out = Path("report.md"); '
        "out.unlink()'"
    )
    assert bash_targets.write_targets(source) == ["report.md"]


@pytest.mark.parametrize(
    ("command", "expected"),
    [
        ("dd if=/dev/zero of=big.bin bs=1M count=1", ["big.bin"]),
        ("grep -r x . | tee -a audit.log", ["audit.log"]),
        ("git checkout -- conductor/handoff.py", ["conductor/handoff.py"]),
        ("git apply fix.patch", ["fix.patch"]),
        ("patch -p1 target.c", ["target.c"]),
        ("cp a.txt b.txt", ["b.txt"]),
        ("mv a.txt b.txt", ["b.txt"]),
        ("install -m 644 a.txt b.txt", ["b.txt"]),
        ("ln -s a.txt b.txt", ["b.txt"]),
        ("rsync -a src/ dest/", ["dest/"]),
        ("truncate -s 0 log.txt", ["log.txt"]),
        ("sed --in-place s/a/b/ conf.ini", ["conf.ini"]),
        # `--` ends flag parsing, so what follows is a path even if it looks
        # like a flag.
        ("rm -f -- --weird-name.txt", ["--weird-name.txt"]),
        # `bash -c` payloads and `bash <<EOF` bodies are commands, not data.
        ('bash -c "echo x > nested.txt"', ["nested.txt"]),
        ("bash <<'EOF'\nsed -i s/a/b/ inner.py\nEOF", ["inner.py"]),
        # A backslash escapes the next character, including a newline.
        ("echo one \\\n  > wrapped.txt", ["wrapped.txt"]),
    ],
)
def test_write_shaped_command_families(bash_targets, command, expected) -> None:
    """One case per write-shaped family the resolver claims to understand."""
    assert bash_targets.write_targets(command) == expected


def test_unparseable_command_falls_back_to_write_shape(bash_targets) -> None:
    """An unbalanced quote defeats the tokenizer. Denying every exotic command
    would break far more than it protects, so the fallback denies only what
    still looks like a write."""
    assert bash_targets.write_targets("echo 'unbalanced > out.txt") == [
        bash_targets.OPAQUE_WRITE
    ]
    assert bash_targets.write_targets("echo 'unbalanced | wc -l") == []


def test_verify_bash_fails_open_when_the_resolver_raises(
    bash_gate, capsys, monkeypatch
) -> None:
    """A deliberate asymmetry with `verify`: this hook sees every Bash call in
    the fleet, so a resolver bug must not deny all of them. Pinned because it is
    the kind of decision a later reader would "fix" into fail-closed."""
    import bash_write_targets

    def boom(command: str, repo_root: Path) -> list[str]:
        raise RuntimeError("resolver bug")

    monkeypatch.setattr(bash_write_targets, "repo_write_targets", boom)
    assert _bash_decision(bash_gate, capsys, "echo x >> a.py") == "allow"


def test_a_heredoc_does_not_disturb_the_write_target(bash_targets) -> None:
    """Both shapes, because both broke. `split_heredocs` removes the BODY but
    leaves `<<` and its delimiter on the command line: as an operand that made
    `tee` report a write to a file called "EOF", and dropping the intro line
    instead would lose the redirect that names the real target."""
    assert bash_targets.write_targets("tee out.txt <<'EOF'\nbody\nEOF") == ["out.txt"]
    assert bash_targets.write_targets(
        "cat > conductor/x.py <<'EOF'\nprint('hi')\nEOF"
    ) == ["conductor/x.py"]
