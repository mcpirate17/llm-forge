#!/usr/bin/env bash
# Shared runtime resolution for legacy shell hook bodies.

hook_python() {
    local root="${1:?repository root is required}"
    local candidate="${HOOK_PYTHON:-$root/.venv/bin/python}"
    if [[ -x "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi
    command -v python3
}

hook_pythonpath() {
    local root="${1:?repository root is required}"
    export PYTHONPATH="$root${PYTHONPATH:+:$PYTHONPATH}"
}
