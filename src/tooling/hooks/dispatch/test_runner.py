"""Runner contracts: raising hooks are loud, timeouts bound, subprocess bodies run."""

from __future__ import annotations

import io
import json
import os
import stat
import sys
import time
from pathlib import Path

import pytest

from tooling.hooks.dispatch import __main__ as dispatch_main
from tooling.hooks.dispatch import adapters, runner
from tooling.hooks.dispatch.registry import HookSpec

PAYLOAD = {"session_id": "t", "tool_name": "Bash", "tool_input": {"command": "echo hi"}}


def _spec(name: str, **kw) -> HookSpec:
    base = {
        "event": "PreToolUse",
        "matcher": "Bash",
        "timeout": 5,
        "legacy_command": "x",
    }
    return HookSpec(name, **{**base, **kw})


def _ctx(tmp_path: Path) -> runner.Context:
    return runner.build_context("PreToolUse", json.dumps(PAYLOAD).encode(), tmp_path)


@pytest.fixture
def fake_adapters(monkeypatch):
    def install(name, fn):
        monkeypatch.setattr(adapters, name, fn, raising=False)

    return install


def test_raising_adapter_is_a_visible_error(tmp_path, fake_adapters):
    def boom(ctx):
        raise RuntimeError("kaput")

    fake_adapters("boom", boom)
    outcome = runner.run_one(_spec("boom", adapter="boom"), _ctx(tmp_path))
    assert outcome.output is None
    assert outcome.error == "RuntimeError: kaput"


def test_print_from_unbound_helper_thread_raises(tmp_path, fake_adapters):
    import threading

    seen: list[BaseException] = []

    def helper():
        try:
            print("lost json")
        except BaseException as exc:  # noqa: BLE001 - the test records it
            seen.append(exc)

    def spawn(ctx):
        worker = threading.Thread(target=helper)
        worker.start()
        worker.join()
        return {"hookSpecificOutput": {}}

    fake_adapters("spawn", spawn)
    outcome = runner.run_one(_spec("spawn", adapter="spawn"), _ctx(tmp_path))
    assert outcome.error is None
    assert len(seen) == 1 and isinstance(seen[0], RuntimeError)
    assert "no bound buffer" in str(seen[0])


def test_adapter_dict_return_is_used_verbatim(tmp_path, fake_adapters):
    fake_adapters(
        "give", lambda ctx: {"hookSpecificOutput": {"permissionDecision": "deny"}}
    )
    outcome = runner.run_one(_spec("give", adapter="give"), _ctx(tmp_path))
    assert outcome.output == {"hookSpecificOutput": {"permissionDecision": "deny"}}
    assert outcome.error is None


def test_printing_adapter_stdout_is_captured_per_thread(tmp_path, fake_adapters):
    def printer(ctx):
        payload = json.load(sys.stdin)
        print(json.dumps({"seen": payload["tool_input"]["command"]}))

    fake_adapters("printer", printer)
    outcomes = runner.run_all(
        (_spec("printer", adapter="printer"),) * 3, _ctx(tmp_path)
    )
    assert [o.output for o in outcomes] == [{"seen": "echo hi"}] * 3
    assert all(o.error is None for o in outcomes)


def test_non_json_stdout_is_an_error(tmp_path, fake_adapters):
    fake_adapters("noisy", lambda ctx: print("Traceback (most recent call last)"))
    outcome = runner.run_one(_spec("noisy", adapter="noisy"), _ctx(tmp_path))
    assert outcome.output is None
    assert outcome.error is not None
    assert "not JSON" in outcome.error


def test_nonzero_system_exit_is_an_error_but_zero_is_not(tmp_path, fake_adapters):
    fake_adapters("exit0", lambda ctx: sys.exit(0))
    fake_adapters("exit3", lambda ctx: sys.exit(3))
    ok = runner.run_one(_spec("exit0", adapter="exit0"), _ctx(tmp_path))
    bad = runner.run_one(_spec("exit3", adapter="exit3"), _ctx(tmp_path))
    assert ok.error is None
    assert bad.error == "SystemExit(3)"


def test_slow_adapter_times_out_without_blocking_the_event(tmp_path, fake_adapters):
    def slow(ctx):
        time.sleep(3)

    fake_adapters("slow", slow)
    fake_adapters("fast", lambda ctx: {"hookSpecificOutput": {}})
    started = time.perf_counter()
    outcomes = runner.run_all(
        (_spec("slow", adapter="slow", timeout=1), _spec("fast", adapter="fast")),
        _ctx(tmp_path),
    )
    assert time.perf_counter() - started < 2.5
    assert outcomes[0].error == "timed out after 1s"
    assert outcomes[1].output == {"hookSpecificOutput": {}}


def _script(tmp_path: Path, name: str, text: str) -> str:
    path = tmp_path / name
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return name


def test_subprocess_python_body_gets_payload_and_env(tmp_path):
    name = _script(
        tmp_path,
        "body.py",
        "import json, os, sys\n"
        "p = json.load(sys.stdin)\n"
        "print(json.dumps({'cmd': p['tool_input']['command'], 'root': os.environ['PROJECT_DIR']}))\n",
    )
    outcome = runner.run_one(_spec("body", argv=(name,)), _ctx(tmp_path))
    assert outcome.error is None
    assert outcome.output == {"cmd": "echo hi", "root": str(tmp_path)}


def test_subprocess_nonzero_exit_is_an_error(tmp_path):
    name = _script(tmp_path, "bad.sh", "#!/bin/bash\necho nope >&2\nexit 7\n")
    outcome = runner.run_one(_spec("bad", argv=(name,)), _ctx(tmp_path))
    assert outcome.output is None
    assert outcome.error == "exit 7: nope"


def test_subprocess_timeout_is_an_error(tmp_path):
    name = _script(tmp_path, "sleep.sh", "#!/bin/bash\nsleep 5\n")
    outcome = runner.run_one(_spec("sleepy", argv=(name,), timeout=1), _ctx(tmp_path))
    assert outcome.error == "timed out after 1s"


def test_workspace_exposure_adapter_emits_exposure_line_verbatim(tmp_path):
    """The SessionStart EXPOSED hook's adapter is exactly ``exposure_line`` --
    nothing may drift between the text the preamble-era line produced and the
    one the native port is parity-tested against."""
    ctx = runner.build_context(
        "SessionStart", json.dumps({"source": "startup"}).encode(), tmp_path
    )
    output = adapters.workspace_exposure(ctx)
    from conductor.workspace_hygiene import exposure_line

    assert output == {
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": exposure_line(tmp_path),
        }
    }
    assert output["hookSpecificOutput"]["additionalContext"].startswith("EXPOSED:")


def test_select_uses_registry_matchers():
    assert [s.name for s in runner.select("PreToolUse", PAYLOAD)] == [
        "crg_refresh_report_pre",
        "crg_gate_verify_bash",
        "pre_bash",
        "current_work_guard_bash",
    ]
    assert [s.name for s in runner.select("SessionStart", {"source": "resume"})] == [
        "crg_refresh_report_session",
        "session_start",
        "workspace_exposure_session",
        "session_handoff",
        "native_freshness",
    ]
    assert [s.name for s in runner.select("PreToolUse", {"tool_name": "Glob"})] == [
        "crg_refresh_report_pre"
    ]
    graph = {"tool_name": "mcp__code-review-graph__locate_tool"}
    assert [s.name for s in runner.select("PreToolUse", graph)] == [
        "crg_gate_mark",
        "crg_refresh_wait",
        "crg_refresh_report_pre",
    ]


def test_select_drops_hooks_named_in_forge_native_hooks(monkeypatch):
    monkeypatch.setenv("FORGE_NATIVE_HOOKS", " pre_bash ,,current_work_guard_bash")
    assert [s.name for s in runner.select("PreToolUse", PAYLOAD)] == [
        "crg_refresh_report_pre",
        "crg_gate_verify_bash",
    ]


def test_select_runs_everything_when_forge_native_hooks_is_unset(monkeypatch):
    monkeypatch.delenv("FORGE_NATIVE_HOOKS", raising=False)
    assert [s.name for s in runner.select("PreToolUse", PAYLOAD)] == [
        "crg_refresh_report_pre",
        "crg_gate_verify_bash",
        "pre_bash",
        "current_work_guard_bash",
    ]


def test_dispatch_merges_and_reports_errors(tmp_path, fake_adapters, monkeypatch):
    fake_adapters(
        "deny",
        lambda ctx: {
            "hookSpecificOutput": {
                "permissionDecision": "deny",
                "permissionDecisionReason": "r",
            }
        },
    )
    fake_adapters("boom", lambda ctx: 1 / 0)
    specs = (_spec("deny", adapter="deny"), _spec("boom", adapter="boom"))
    monkeypatch.setattr(runner, "hooks_for", lambda event: specs)
    result, outcomes = runner.dispatch(
        "PreToolUse", json.dumps(PAYLOAD).encode(), tmp_path
    )
    assert result["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert "HOOK ERROR [boom]: ZeroDivisionError" in result["systemMessage"]
    assert os.environ["PROJECT_DIR"] == str(tmp_path)
    assert len(outcomes) == 2


def test_malformed_payload_dispatches_with_empty_payload(tmp_path, monkeypatch):
    monkeypatch.setattr(runner, "hooks_for", lambda event: ())
    result, outcomes = runner.dispatch("PostToolUse", b"not json", tmp_path)
    assert result == {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    assert outcomes == []


def test_codex_pretooluse_normalization_is_host_specific():
    bare_allow = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "benign",
        }
    }
    assert (
        dispatch_main._normalize_output("claude", "PreToolUse", bare_allow)
        == bare_allow
    )
    assert dispatch_main._normalize_output("codex", "PreToolUse", bare_allow) == {}

    with_system_message = {**bare_allow, "systemMessage": "kept", "ignored": True}
    assert dispatch_main._normalize_output(
        "codex", "PreToolUse", with_system_message
    ) == {"systemMessage": "kept"}

    rewrite = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {"command": "echo rewritten"},
        }
    }
    assert dispatch_main._normalize_output("codex", "PreToolUse", rewrite) == rewrite

    deny = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "blocked",
        }
    }
    assert dispatch_main._normalize_output("codex", "PreToolUse", deny) == deny


def test_codex_pretooluse_turns_unsupported_ask_into_deny():
    result = dispatch_main._normalize_output(
        "codex",
        "PreToolUse",
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": "review this",
            }
        },
    )
    assert result["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert result["hookSpecificOutput"]["permissionDecisionReason"] == "review this"

    without_reason = dispatch_main._normalize_output(
        "codex",
        "PreToolUse",
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
            }
        },
    )
    assert without_reason["hookSpecificOutput"]["permissionDecisionReason"] == (
        "Hook requested approval, but Codex PreToolUse hooks do not support ask."
    )


def test_codex_posttooluse_drops_unsupported_rewrites_and_suppression():
    result = dispatch_main._normalize_output(
        "codex",
        "PostToolUse",
        {
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": {"output": "bounded"},
                "updatedMCPToolOutput": {"content": []},
            },
            "suppressOutput": True,
            "systemMessage": "kept",
        },
    )
    assert result == {"systemMessage": "kept"}

    supported = {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "reviewed",
        },
        "decision": "block",
        "reason": "needs review",
        "continue": False,
        "stopReason": "replace output",
    }
    assert (
        dispatch_main._normalize_output("codex", "PostToolUse", supported) == supported
    )


def test_main_applies_explicit_codex_protocol_before_stdout(
    tmp_path, monkeypatch, capsys
):
    raw = b'{"turn_id":"turn-test"}'
    monkeypatch.setattr(sys, "stdin", io.TextIOWrapper(io.BytesIO(raw)))
    stdout = io.StringIO()
    monkeypatch.setattr(sys, "stdout", stdout)
    roots = []

    def _dispatch(event, payload, root):
        roots.append(root)
        return (
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                }
            },
            [
                runner.HookOutcome(
                    name="json-hook", output={"ok": True}, elapsed_ms=1.0
                ),
                runner.HookOutcome(name="quiet-hook", output=None, elapsed_ms=1.0),
                runner.HookOutcome(
                    name="error-hook", output=None, error="boom", elapsed_ms=1.0
                ),
            ],
        )

    monkeypatch.setattr(
        dispatch_main.runner,
        "dispatch",
        _dispatch,
    )
    monkeypatch.setattr(dispatch_main, "_record_hook_timings", lambda *args: None)
    monkeypatch.setenv("HOOK_DISPATCH_TRACE", "1")

    assert (
        dispatch_main.main(
            ["PreToolUse", "--protocol", "codex", "--project-dir", str(tmp_path)]
        )
        == 0
    )
    assert json.loads(stdout.getvalue()) == {}
    assert stdout.getvalue().endswith("\n")
    assert roots == [tmp_path.resolve()]
    trace = capsys.readouterr().err.splitlines()
    assert any(
        "[dispatch] json-hook" in line and line.endswith("  json") for line in trace
    )
    assert any(
        "[dispatch] quiet-hook" in line and line.endswith("  quiet") for line in trace
    )
    assert any(
        "[dispatch] error-hook" in line and line.endswith("  boom") for line in trace
    )


def test_main_settings_prints_the_registry_block(monkeypatch):
    stdout = io.StringIO()
    monkeypatch.setattr(sys, "stdout", stdout)
    monkeypatch.setattr(dispatch_main, "settings_block", lambda: {"hooks": "ok"})

    assert dispatch_main.main(["settings"]) == 0
    assert json.loads(stdout.getvalue()) == {"hooks": "ok"}


def test_record_hook_timings_labels_quiet_outcomes(monkeypatch):
    from conductor import context_telemetry

    events = []
    monkeypatch.setattr(
        context_telemetry, "record", lambda event, path: events.append(event)
    )

    dispatch_main._record_hook_timings(
        "PostToolUse",
        [runner.HookOutcome(name="quiet-hook", output=None, elapsed_ms=1.0)],
        "session",
    )

    assert events[0]["status"] == "quiet"


def _splice_specs(fake_adapters, calls: list[str]) -> tuple[HookSpec, ...]:
    """Three PreToolUse/Bash specs -- `a`, `pre_bash`, `b` -- in registry order,
    each recording its name in `calls` if its adapter actually runs. Shared by
    every native-splice test below so each only sets the one env var it means
    to exercise.
    """
    for name in ("a", "pre_bash", "b"):
        fake_adapters(
            name, lambda ctx, name=name: (calls.append(name), {"ok": name})[1]
        )
    return (
        _spec("a", adapter="a"),
        _spec("pre_bash", adapter="pre_bash"),
        _spec("b", adapter="b"),
    )


def test_dispatch_splices_a_native_answer_back_in_at_its_registry_position(
    tmp_path, fake_adapters, monkeypatch
):
    calls: list[str] = []
    specs = _splice_specs(fake_adapters, calls)
    monkeypatch.setattr(runner, "hooks_for", lambda event: specs)
    native_answer = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "native deny",
        }
    }
    monkeypatch.setenv("FORGE_NATIVE_HOOKS", "pre_bash")
    monkeypatch.setenv("FORGE_NATIVE_ANSWERS", json.dumps({"pre_bash": native_answer}))

    result, outcomes = runner.dispatch(
        "PreToolUse", json.dumps(PAYLOAD).encode(), tmp_path
    )

    assert [o.name for o in outcomes] == ["a", "pre_bash", "b"]
    pre_bash_outcome = outcomes[1]
    assert pre_bash_outcome.output == native_answer
    assert pre_bash_outcome.error is None
    assert calls == ["a", "b"], "pre_bash's adapter must never run once served natively"
    assert result["hookSpecificOutput"]["permissionDecision"] == "deny"


def test_dispatch_raises_loud_when_a_served_hook_has_no_native_answer(
    tmp_path, fake_adapters, monkeypatch
):
    calls: list[str] = []
    specs = _splice_specs(fake_adapters, calls)
    monkeypatch.setattr(runner, "hooks_for", lambda event: specs)
    monkeypatch.setenv("FORGE_NATIVE_HOOKS", "pre_bash")
    monkeypatch.delenv("FORGE_NATIVE_ANSWERS", raising=False)

    with pytest.raises(ValueError, match="pre_bash"):
        runner.dispatch("PreToolUse", json.dumps(PAYLOAD).encode(), tmp_path)


def test_dispatch_runs_everything_in_python_when_forge_native_hooks_is_empty(
    tmp_path, fake_adapters, monkeypatch
):
    calls: list[str] = []
    specs = _splice_specs(fake_adapters, calls)
    monkeypatch.setattr(runner, "hooks_for", lambda event: specs)
    monkeypatch.setenv("FORGE_NATIVE_HOOKS", "")
    monkeypatch.delenv("FORGE_NATIVE_ANSWERS", raising=False)

    _, outcomes = runner.dispatch("PreToolUse", json.dumps(PAYLOAD).encode(), tmp_path)

    assert calls == ["a", "pre_bash", "b"], "the escape hatch must run every hook"
    assert [o.name for o in outcomes] == ["a", "pre_bash", "b"]
