#!/usr/bin/env bash
# SessionStart: surface graph stats AND inject a binding rule that any Edit/Write
# this session must be preceded by a code-review-graph tool call. This is the
# enforcement layer for CLAUDE.md's "use code review graph to understand all
# 20K files". Prior incidents (codex 2026-04-29) duplicated existing functions
# and mutated god files because the agent skipped the graph entirely.
#
# This file is the GENERIC half and carries no repo-specific knowledge. Anything
# a particular repo wants at session start -- which scratch dirs expire, which
# ingest jobs run, what extra context to inject -- lives in the project
# extension .claude/hooks/project/session-start.sh (see AGENTS.md, "Hook
# extension convention"). The hook runs correctly when that file is absent.

set -euo pipefail

HOOK_DIR="$(dirname "$(readlink -f "$0")")"
# The checkout this hook serves: the launcher passes PROJECT_DIR; run directly,
# the body sits at <root>/tooling/hooks/claude/.
REPO_ROOT="${PROJECT_DIR:-$(dirname "$(dirname "$(dirname "$HOOK_DIR")")")}"
PROJECT_HOOK_DIR="${PROJECT_HOOK_DIR:-$REPO_ROOT/.claude/hooks/project}"

HOOK_INPUT=$(cat)

# Project extension point: same stdin as this hook, stdout appended to the
# injected context by _append_context.py, stderr passed through. Additive only
# -- it cannot shrink or replace what the generic half produced -- and a
# non-zero exit is reported, never fatal.
# Its stdout is collected through a file, never a `$(...)` pipe: a pipe stays
# open until the extension's last DETACHED child exits, so one background job
# (obsidian_ops_cycle, 2026-09-03) held session start for 1.2 s.
PROJECT_HOOK="$PROJECT_HOOK_DIR/session-start.sh"
PROJECT_HOOK_CONTEXT=""
if [[ -x "$PROJECT_HOOK" ]]; then
  PROJECT_HOOK_OUT=$(mktemp)
  if printf '%s' "$HOOK_INPUT" \
    | REPO_ROOT="$REPO_ROOT" HOOK_DIR="$HOOK_DIR" "$PROJECT_HOOK" >"$PROJECT_HOOK_OUT"; then
    PROJECT_HOOK_CONTEXT=$(<"$PROJECT_HOOK_OUT")
  else
    echo "[session-start] project extension failed: $PROJECT_HOOK" >&2
  fi
  rm -f "$PROJECT_HOOK_OUT"
fi
export PROJECT_HOOK_CONTEXT

# Auto-refresh active state (AVO Tier-0 state cache)
( cd "$REPO_ROOT" && python3 -m conductor.active_state update >/dev/null 2>&1 & ) || true

# A2A: ensure this identity's endpoint is serving, retry its queued sends, and
# surface a bounded (<=1200 chars) unread preview in the injected context via
# conductor.a2a_session_start — it never falls back to the full inbox. Identity:
# A2A_AGENT_NAME, else .agents/a2a/default_identity (see _identity.sh). Delivery
# is synchronous, so a session that never serves can never receive — this is the
# receive half of A2A. Failures are reported in the context, never swallowed.
# shellcheck source=/dev/null
source "$HOOK_DIR/_identity.sh"
A2A_SUMMARY=""
A2A_PY="$REPO_ROOT/.venv/bin/python"
if [[ -z "$A2A_ID" ]]; then
  echo "[session-start] a2a: no identity (export A2A_AGENT_NAME or write .agents/a2a/default_identity); inbox preview skipped" >&2
elif [[ -x "$A2A_PY" ]]; then
  A2A_LOG="$REPO_ROOT/.agents/a2a/session-start.log"
  mkdir -p "$REPO_ROOT/.agents/a2a"
  if ! A2A_SUMMARY=$(cd "$REPO_ROOT" && "$A2A_PY" -m conductor.a2a_session_start --provider claude \
      --identity "$A2A_ID" --output text --serve-timeout 3 2>>"$A2A_LOG"); then
    A2A_SUMMARY="A2A preview unavailable for '$A2A_ID': $(tail -n1 "$A2A_LOG" 2>/dev/null || true)"
  fi
fi

printf '%s' "$HOOK_INPUT" | PROJECT_DIR="$REPO_ROOT" "$(dirname "$HOOK_DIR")/agent/crg_gate.py" start

export PYTHONPATH="$REPO_ROOT${PYTHONPATH:+:$PYTHONPATH}"
export A2A_SUMMARY="${A2A_SUMMARY:-}"
# hook-context records the injected additionalContext size (bytes only) and
# passes the JSON through untouched, so hook cost shows up in the telemetry.
python3 -m conductor.session_preamble hook --a2a-name "$A2A_ID" \
  | python3 "$HOOK_DIR/_append_context.py" \
  | python3 -m conductor.context_telemetry hook-context --hook session-start
