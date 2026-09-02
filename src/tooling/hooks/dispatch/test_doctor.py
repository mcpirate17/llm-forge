"""Hook doctor: every class of silently dead hook is reported DEAD, never OK."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tooling.hooks.dispatch import doctor

GOOD = '#!/usr/bin/env python3\nimport json\nprint(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse"}}))\n'


def _hook(project: Path, name: str, text: str, executable: bool = True) -> Path:
    path = project / ".claude" / "hooks" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755 if executable else 0o644)
    return path


def _declared(
    name: str, event: str = "PreToolUse", matcher: str = "Bash", timeout: int = 3
) -> doctor.Declared:
    return doctor.Declared(
        event, matcher, f"$CLAUDE_PROJECT_DIR/.claude/hooks/{name}", timeout
    )


def _static(project: Path, declared: doctor.Declared) -> doctor.Report:
    report = doctor.Report(declared)
    doctor.static_checks(doctor.resolve(declared.command, project), report, project)
    return report


def _run(project: Path, declared: doctor.Declared, scratch: Path) -> doctor.Report:
    report = doctor.Report(declared)
    doctor.run_check(declared, report, project, scratch)
    return report


def test_good_hook_passes_static_and_run(tmp_path):
    _hook(tmp_path, "good.py", GOOD)
    declared = _declared("good.py")
    assert _static(tmp_path, declared).status == "OK"
    report = _run(tmp_path, declared, tmp_path / "scratch")
    assert report.status == "OK"
    assert report.elapsed_ms > 0


def test_mode_644_hook_is_dead_and_exits_126(tmp_path):
    """The read_budget.py class: harness ran a non-executable file for days."""
    _hook(tmp_path, "budget.py", GOOD, executable=False)
    declared = _declared("budget.py")
    static = _static(tmp_path, declared)
    assert static.status == "DEAD"
    assert "not executable (mode 644)" in static.problems[0]
    run = _run(tmp_path, declared, tmp_path / "scratch")
    assert run.status == "DEAD"
    assert run.problems[0].startswith("exit 126")


def test_nonzero_exit_with_no_output_is_dead(tmp_path):
    _hook(tmp_path, "crash.py", "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n")
    report = _run(tmp_path, _declared("crash.py"), tmp_path / "scratch")
    assert report.status == "DEAD"
    assert report.problems == ["exit 1; stderr: <empty>"]


def test_garbage_stdout_is_dead(tmp_path):
    _hook(
        tmp_path,
        "garbage.sh",
        "#!/bin/bash\necho 'Traceback (most recent call last):'\n",
    )
    report = _run(tmp_path, _declared("garbage.sh"), tmp_path / "scratch")
    assert report.status == "DEAD"
    assert report.problems[0].startswith("stdout is not JSON")


def test_silent_hook_warns_unless_registered_quiet(tmp_path):
    _hook(tmp_path, "silent.sh", "#!/bin/bash\nexit 0\n")
    report = _run(tmp_path, _declared("silent.sh"), tmp_path / "scratch")
    assert report.status == "WARN"
    assert report.problems == ["wrote nothing to stdout"]


def test_timeout_is_dead(tmp_path):
    _hook(tmp_path, "slow.sh", "#!/bin/bash\nsleep 5\n")
    report = _run(tmp_path, _declared("slow.sh", timeout=1), tmp_path / "scratch")
    assert report.status == "DEAD"
    assert report.problems == ["timed out after 1s"]


def test_missing_empty_and_broken_scripts_are_dead(tmp_path):
    _hook(tmp_path, "empty.py", "")
    _hook(tmp_path, "noshebang.py", "print(1)\n")
    _hook(tmp_path, "syntax.py", "#!/usr/bin/env python3\ndef (:\n")
    assert _static(tmp_path, _declared("absent.py")).problems[0].startswith("missing:")
    assert (
        _static(tmp_path, _declared("empty.py")).problems[0].startswith("empty file:")
    )
    assert (
        _static(tmp_path, _declared("noshebang.py"))
        .problems[0]
        .startswith("no shebang:")
    )
    assert (
        _static(tmp_path, _declared("syntax.py"))
        .problems[0]
        .startswith("does not compile:")
    )


def test_resolve_strips_env_prefix_and_interpreter(tmp_path):
    resolved = doctor.resolve(
        'env GOVERNANCE_OWNER="${GOVERNANCE_OWNER:-claude}" $CLAUDE_PROJECT_DIR/x.py verify',
        tmp_path,
    )
    assert resolved.env == {"GOVERNANCE_OWNER": "${GOVERNANCE_OWNER:-claude}"}
    assert resolved.script == tmp_path / "x.py"
    assert resolved.interpreter is None
    module = doctor.resolve("python3 -m conductor.context_telemetry", tmp_path)
    assert module.module == "conductor.context_telemetry"
    assert module.interpreter == "python3"


def test_unregistered_command_is_dead_and_main_exits_one(tmp_path, capsys):
    _hook(tmp_path, "rogue.py", GOOD)
    settings = tmp_path / ".claude" / "settings.json"
    settings.write_text(
        json.dumps(
            {
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "Bash",
                            "hooks": [
                                {
                                    "type": "command",
                                    "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/rogue.py",
                                    "timeout": 3,
                                }
                            ],
                        }
                    ]
                }
            }
        )
    )
    assert doctor.main(["--project-dir", str(tmp_path)]) == 1
    out = capsys.readouterr().out
    assert "DEAD" in out
    assert "resolves to no registered hook" in out
    assert out.rstrip().endswith("hook-doctor | FAIL dead=1 warn=0 total=1")


def test_main_without_settings_fails(tmp_path, capsys):
    assert doctor.main(["--project-dir", str(tmp_path)]) == 1
    assert "no settings file" in capsys.readouterr().err


@pytest.mark.parametrize("event", ("SessionStart", "SessionEnd"))
def test_session_events_get_a_synthetic_payload(tmp_path, event):
    _hook(
        tmp_path,
        "s.sh",
        '#!/bin/bash\ncat >/dev/null; echo \'{"hookSpecificOutput":{"hookEventName":"'
        + event
        + "\"}}'\n",
    )
    report = _run(
        tmp_path, _declared("s.sh", event=event, matcher=""), tmp_path / "scratch"
    )
    assert report.status == "OK"
