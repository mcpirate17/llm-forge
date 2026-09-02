"""Contracts for the code-review-graph MCP stdio probe.

Every case drives the real probe against a fake stdio MCP server: a small Python
script spawned as a subprocess that speaks ``initialize``,
``notifications/initialized``, ``tools/list`` and ``tools/call``. Its behaviour is
steered by environment variables, so the probe under test is never mocked.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from conductor import crg_mcp_probe as probe_mod

FAKE_SERVER = """
import json, os, sys

TOOLS = int(os.environ.get("FAKE_TOOLS", "3"))
LEAK = os.environ.get("FAKE_LEAK", "")
CALL_ERROR = os.environ.get("FAKE_CALL_ERROR", "")
REQUIRE_NOTIFY = os.environ.get("FAKE_REQUIRE_NOTIFY", "1") == "1"

notified = False


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\\n")
    sys.stdout.flush()


while True:
    line = sys.stdin.readline()
    if not line:
        break
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "notifications/initialized":
        notified = True
        continue
    if method == "initialize":
        emit({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "serverInfo": {"name": "fake", "version": "0"}}})
    elif method == "tools/list":
        if REQUIRE_NOTIFY and not notified:
            emit({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32002, "message": "not initialized"}})
        else:
            emit({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "tools": [{"name": "tool_%d" % i} for i in range(TOOLS)]}})
    elif method == "tools/call":
        if CALL_ERROR:
            emit({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32000, "message": "boom"}})
        else:
            emit({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "content": [{"type": "text", "text": LEAK or "42 nodes"}]}})
"""


@pytest.fixture()
def server(tmp_path: Path) -> Path:
    script = tmp_path / "fake_mcp_server.py"
    script.write_text(FAKE_SERVER)
    return script


def run_probe(server: Path, tmp_path: Path, env: dict[str, str], **kwargs):
    return probe_mod.probe(
        [sys.executable, str(server)],
        tmp_path,
        env,
        kwargs.pop("call", ("stats_tool", {})),
        timeout=kwargs.pop("timeout", 30.0),
        **kwargs,
    )


def test_probe_completes_the_handshake_and_reports_the_listed_tools(
    server: Path, tmp_path: Path
) -> None:
    report = run_probe(server, tmp_path, {"FAKE_TOOLS": "4"}, expect_tools=4)
    assert report["tools"] == ["tool_0", "tool_1", "tool_2", "tool_3"]
    assert report["call"] == "stats_tool"


def test_probe_fails_when_the_tool_count_differs_from_the_expected_one(
    server: Path, tmp_path: Path
) -> None:
    with pytest.raises(
        probe_mod.ProbeError, match="expected 22 tools, server listed 3"
    ):
        run_probe(server, tmp_path, {}, expect_tools=22)


def test_probe_fails_when_a_result_string_carries_an_absolute_path(
    server: Path, tmp_path: Path
) -> None:
    leak = "/home/tim/Projects/LLM/conductor/gate.py:12"
    with pytest.raises(probe_mod.ProbeError, match="absolute paths in results"):
        run_probe(server, tmp_path, {"FAKE_LEAK": leak}, expect_tools=3)
    clean = run_probe(server, tmp_path, {"FAKE_LEAK": "conductor/gate.py:12"})
    assert clean["needles"] == ["/home/"]


def test_probe_surfaces_a_jsonrpc_error_from_the_tool_call(
    server: Path, tmp_path: Path
) -> None:
    with pytest.raises(probe_mod.ProbeError, match="tools/call: .*boom"):
        run_probe(server, tmp_path, {"FAKE_CALL_ERROR": "1"})


def test_find_absolute_paths_scans_keys_and_nested_values(tmp_path: Path) -> None:
    result = {
        "content": [{"type": "text", "text": "see /home/tim/x.py"}],
        "meta": {"/home/tim/root": ["nested", {"deep": "/srv/other"}]},
    }
    hits = probe_mod.find_absolute_paths(result, ["/home/"])
    assert len(hits) == 2
    assert probe_mod.find_absolute_paths({"a": ["relative/x.py"]}, ["/home/"]) == []
    assert probe_mod.walk_strings({"k": [1, None, "v"]}) == ["k", "v"]


def test_load_server_cmd_reads_the_declared_command_args_cwd_and_env(
    tmp_path: Path,
) -> None:
    (tmp_path / ".mcp.json").write_text(
        json.dumps(
            {
                "mcpServers": {
                    "code-review-graph": {
                        "command": "/usr/bin/python3",
                        "args": ["-m", "conductor.crg_server", "--repo", str(tmp_path)],
                        "cwd": str(tmp_path),
                        "env": {"CRG_ROLE": "review"},
                    }
                }
            }
        )
    )
    argv, cwd, env = probe_mod.load_server_cmd(tmp_path)
    assert argv == [
        "/usr/bin/python3",
        "-m",
        "conductor.crg_server",
        "--repo",
        str(tmp_path),
    ]
    assert cwd == tmp_path
    assert env == {"CRG_ROLE": "review"}
    with pytest.raises(probe_mod.ProbeError, match="no .*\\.mcp\\.json"):
        probe_mod.load_server_cmd(tmp_path / "missing")


def test_main_prints_a_pass_verdict_and_exits_zero(
    server: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = probe_mod.main(
        [
            "--server-cmd",
            sys.executable,
            str(server),
            "--repo",
            str(tmp_path),
            "--expect-tools",
            "3",
            "--call",
            "stats_tool",
            "{}",
        ]
    )
    out = capsys.readouterr()
    assert code == 0
    assert out.out.strip() == (
        "crg-probe | PASS 3 tools, call=stats_tool, no absolute paths in results"
    )


def test_main_prints_a_fail_verdict_and_exits_nonzero(
    server: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = probe_mod.main(
        [
            "--server-cmd",
            sys.executable,
            str(server),
            "--repo",
            str(tmp_path),
            "--expect-tools",
            "9",
        ]
    )
    err = capsys.readouterr().err
    assert code == 1
    assert err.startswith("crg-probe | FAIL expected 9 tools")
