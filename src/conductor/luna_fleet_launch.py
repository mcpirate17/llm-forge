#!/usr/bin/env python3
"""Assign the next free fleet identity and print its personalized launch prompt.

The A2A port bind is the lock: the launcher starts ``agent_a2a serve`` for the
lowest registered identity of the requested pool whose port is free, waits until
that process is listening, and only then renders the prompt template with the
identity, port, pid, and shard size filled in. A concurrent launcher that loses
the bind race moves on to the next identity. ``--release`` stops the serve and
removes its pid file so the identity can be reused.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

from conductor.agent_a2a import BIND_HOST, DEFAULT_STATE_DIR, A2aError, load_registry

ROOT = Path(__file__).resolve().parent.parent
POOLS: dict[str, str] = {
    "luna": "luna-",
    "glm": "glm-flash-",
    "minimax": "minimax-",
    "antigravity": "antigravity",
    "terra": "terra-",
    "sonnet": "sonnet-",
    "nemotron": "nemotron-",
}
TEMPLATE = ROOT / "tasks/audit/luna_fleet_prompt_template_20260826.txt"
SHARDS = ROOT / "tasks/audit/luna_fleet_shards_20260826.json"
BIND_WAIT_SECONDS = 5.0


def port_open(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.2)
        return probe.connect_ex((BIND_HOST, port)) == 0


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def start_serve(name: str, port: int, state_dir: Path) -> int | None:
    log_path = state_dir / f"{name}.log"
    pid_path = state_dir / f"{name}.pid"
    with log_path.open("ab") as log:
        process = subprocess.Popen(
            [sys.executable, "-m", "conductor.agent_a2a", "serve", "--name", name],
            cwd=ROOT,
            stdout=log,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
    deadline = time.monotonic() + BIND_WAIT_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return None
        if port_open(port):
            pid_path.write_text(f"{process.pid}\n")
            return process.pid
        time.sleep(0.1)
    process.kill()
    return None


def acquire(
    pool: str | None, state_dir: Path, exact: str | None = None
) -> tuple[str, int, int]:
    registry = load_registry(state_dir)
    shards = json.loads(SHARDS.read_text())["shards"]
    if exact is not None:
        if exact not in registry or exact not in shards:
            raise A2aError(f"{exact!r} is not a registered identity with a shard")
        candidates = [exact]
    else:
        prefix = POOLS[pool or ""]
        candidates = sorted(
            name for name in registry if name.startswith(prefix) and name in shards
        )
    if not candidates:
        raise A2aError(f"no identities with a shard registered under pool {pool!r}")
    for name in candidates:
        port = registry[name].port
        if port_open(port):
            continue
        pid = start_serve(name, port, state_dir)
        if pid is not None:
            return name, port, pid
    raise A2aError(f"every candidate identity is busy: {candidates}")


def render(name: str, port: int, pid: int) -> str:
    shards = json.loads(SHARDS.read_text())["shards"]
    if name not in shards:
        raise A2aError(f"{name} has no shard in {SHARDS}")
    template = TEMPLATE.read_text()
    return (
        template.replace("<AGENT>", name)
        .replace("<PORT>", str(port))
        .replace("<PID>", str(pid))
        .replace("<SHARD_COUNT>", str(len(shards[name]["files"])))
    )


def release(name: str, state_dir: Path) -> str:
    pid_path = state_dir / f"{name}.pid"
    if not pid_path.is_file():
        raise A2aError(f"{name} has no pid file; nothing to release")
    pid = int(pid_path.read_text().strip())
    if pid_alive(pid):
        os.kill(pid, signal.SIGTERM)
    pid_path.unlink()
    return f"released {name} (pid {pid})"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument(
        "--pool", choices=sorted(POOLS), help="acquire the next free identity"
    )
    group.add_argument(
        "--as", dest="exact", metavar="NAME", help="acquire this exact identity"
    )
    group.add_argument(
        "--name", help="re-print the prompt for an identity already serving"
    )
    group.add_argument("--release", metavar="NAME", help="stop an identity's serve")
    parser.add_argument("--state-dir", type=Path, default=DEFAULT_STATE_DIR)
    args = parser.parse_args(argv)
    try:
        if args.release:
            print(release(args.release, args.state_dir))
            return 0
        if args.name:
            registry = load_registry(args.state_dir)
            if args.name not in registry:
                raise A2aError(f"unknown identity {args.name!r}")
            pid_path = args.state_dir / f"{args.name}.pid"
            if not pid_path.is_file() or not port_open(registry[args.name].port):
                raise A2aError(f"{args.name} is not serving; use --pool to acquire")
            pid = int(pid_path.read_text().strip())
            print(render(args.name, registry[args.name].port, pid))
            return 0
        name, port, pid = acquire(args.pool, args.state_dir, exact=args.exact)
        print(render(name, port, pid))
        return 0
    except A2aError as exc:
        print(f"luna_fleet_launch: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
