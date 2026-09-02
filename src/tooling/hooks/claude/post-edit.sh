#!/usr/bin/env bash
# PostToolUse/Edit|Write: auto-format the edited file, then a structural audit.
# The logic lives once, in _post_edit_audit.py, which the dispatcher imports
# directly; this legacy launcher runs the same body.
set -euo pipefail
HERE="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
exec python3 "$HERE/_post_edit_audit.py"
