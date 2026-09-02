#!/usr/bin/env bash
# PreToolUse/Read: deny a whole-file Read of a code file >= 400 lines (the
# KB-OPS-CTX-01 rule). The logic lives once, in _pre_read_skeleton.py, which the
# dispatcher imports directly; this legacy launcher runs the same body and logs
# the nudge size through context_telemetry hook-context (a pass-through).
set -euo pipefail
HERE="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
python3 "$HERE/_pre_read_skeleton.py" | python3 -m conductor.context_telemetry hook-context --hook pre-read-skeleton
