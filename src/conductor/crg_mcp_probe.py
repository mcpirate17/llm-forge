"""Probe the code-review-graph MCP server over stdio and assert its answers are clean.

    python -m conductor.crg_mcp_probe [--server-cmd CMD ...] [--expect-tools N]
                                      [--call TOOL JSON] [--repo ROOT]

Speaks the real handshake — ``initialize``, ``notifications/initialized``,
``tools/list``, one ``tools/call`` — against the server command declared in
``.mcp.json`` (or an explicit ``--server-cmd``). Exits 0 only when all three
requests succeed, the tool count matches ``--expect-tools`` when given, and no
string in either result leaks an absolute path (the repo root, or any ``/home/``
path): a graph answer that carries host paths burns context and pins the reader's
checkout into the response. Prints a one-line ``crg-probe | PASS/FAIL`` verdict.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
from pathlib import Path

SERVER_NAME = "code-review-graph"
DEFAULT_CALL = ("list_graph_stats_tool", "{}")
PROTOCOL_VERSION = "2024-11-05"


class ProbeError(RuntimeError):
    """The probe could not complete, or the server answered unacceptably."""


def load_server_cmd(repo_root: Path) -> tuple[list[str], Path, dict[str, str]]:
    """Read the code-review-graph stdio server command out of the repo's .mcp.json."""
    config = repo_root / ".mcp.json"
    if not config.is_file():
        raise ProbeError(f"no {config}; pass --server-cmd explicitly")
    servers = json.loads(config.read_text()).get("mcpServers", {})
    entry = servers.get(SERVER_NAME)
    if entry is None:
        raise ProbeError(f"{config} declares no {SERVER_NAME!r} server")
    argv = [entry["command"], *entry.get("args", [])]
    cwd = Path(entry.get("cwd", repo_root))
    return argv, cwd, dict(entry.get("env", {}))


def _pump(stream, sink: queue.Queue) -> None:
    for line in stream:
        sink.put(line)
    sink.put(None)


class StdioClient:
    """A minimal JSON-RPC-over-stdio MCP client: send a request, await its id."""

    def __init__(self, argv: list[str], cwd: Path, env: dict[str, str]) -> None:
        self.proc = subprocess.Popen(
            argv,
            cwd=str(cwd),
            env={**os.environ, **env},
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._lines: queue.Queue = queue.Queue()
        self._errors: list[str] = []
        threading.Thread(
            target=_pump, args=(self.proc.stdout, self._lines), daemon=True
        ).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()

    def _drain_stderr(self) -> None:
        for line in self.proc.stderr:
            self._errors.append(line)

    def send(self, message: dict) -> None:
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, request_id: int, method: str, params: dict, timeout: float):
        self.send(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        )
        while True:
            try:
                line = self._lines.get(timeout=timeout)
            except queue.Empty:
                raise ProbeError(
                    f"{method}: no response within {timeout:.0f}s"
                ) from None
            if line is None:
                raise ProbeError(
                    f"{method}: server closed stdout ({self.stderr_tail()})"
                )
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise ProbeError(f"{method}: {message['error']}")
            return message["result"]

    def stderr_tail(self, limit: int = 400) -> str:
        return "".join(self._errors)[-limit:].strip()

    def close(self) -> None:
        self.proc.kill()
        self.proc.wait(timeout=10)


def walk_strings(value) -> list[str]:
    """Every string reachable in a decoded JSON value, keys included."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [s for k, v in value.items() for s in ([k] + walk_strings(v))]
    if isinstance(value, (list, tuple)):
        return [s for item in value for s in walk_strings(item)]
    return []


def find_absolute_paths(result, needles: list[str]) -> list[str]:
    """Strings in a tool result that leak one of the forbidden absolute prefixes."""
    return sorted(
        {
            f"{needle} in {text[:120]!r}"
            for text in walk_strings(result)
            for needle in needles
            if needle in text
        }
    )


def probe(
    argv: list[str],
    cwd: Path,
    env: dict[str, str],
    call: tuple[str, dict],
    expect_tools: int | None = None,
    needles: list[str] | None = None,
    timeout: float = 90.0,
) -> dict[str, object]:
    """Run the handshake and the checks. Raises ProbeError on any failure."""
    needles = ["/home/"] if needles is None else needles
    client = StdioClient(argv, cwd, env)
    try:
        client.request(
            1,
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "crg-probe", "version": "1"},
            },
            timeout,
        )
        client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        listed = client.request(2, "tools/list", {}, timeout)
        tools = sorted(tool["name"] for tool in listed["tools"])
        if expect_tools is not None and len(tools) != expect_tools:
            raise ProbeError(
                f"expected {expect_tools} tools, server listed {len(tools)}"
            )
        tool_name, arguments = call
        called = client.request(
            3, "tools/call", {"name": tool_name, "arguments": arguments}, timeout
        )
    finally:
        client.close()
    leaks = find_absolute_paths(listed, needles) + find_absolute_paths(called, needles)
    if leaks:
        raise ProbeError(f"absolute paths in results: {'; '.join(leaks[:3])}")
    return {"tools": tools, "call": tool_name, "needles": needles}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m conductor.crg_mcp_probe",
        description="Handshake with the code-review-graph MCP server and check its answers.",
    )
    parser.add_argument(
        "--server-cmd",
        nargs="+",
        default=None,
        help="server argv (default: the code-review-graph entry in .mcp.json)",
    )
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="repo root")
    parser.add_argument("--expect-tools", type=int, default=None, help="required count")
    parser.add_argument(
        "--call",
        nargs=2,
        metavar=("TOOL", "JSON"),
        default=list(DEFAULT_CALL),
        help="tool to call and its JSON arguments",
    )
    parser.add_argument(
        "--timeout", type=float, default=90.0, help="per-request seconds"
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    repo = args.repo.resolve()
    try:
        if args.server_cmd:
            cmd, cwd, env = list(args.server_cmd), repo, {}
        else:
            cmd, cwd, env = load_server_cmd(repo)
        report = probe(
            cmd,
            cwd,
            env,
            (args.call[0], json.loads(args.call[1])),
            expect_tools=args.expect_tools,
            needles=sorted({"/home/", f"{repo}/"}),
            timeout=args.timeout,
        )
    except ProbeError as exc:
        print(f"crg-probe | FAIL {exc}", file=sys.stderr)
        return 1
    print(
        f"crg-probe | PASS {len(report['tools'])} tools, "
        f"call={report['call']}, no absolute paths in results"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
