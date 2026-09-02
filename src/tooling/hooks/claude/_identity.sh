# shellcheck shell=bash
# Sourced by SessionStart hooks. Resolves this session's A2A identity into A2A_ID:
#   1. $A2A_AGENT_NAME (exported by the agent's bootstrap), else
#   2. the first line of $REPO_ROOT/.agents/a2a/default_identity (per-checkout,
#      gitignored; write the registered name there once for sessions that never
#      export the variable), else empty.
# Requires REPO_ROOT. Never fails the caller.
A2A_ID="${A2A_AGENT_NAME:-}"
if [[ -z "$A2A_ID" && -r "$REPO_ROOT/.agents/a2a/default_identity" ]]; then
  A2A_ID=$(head -n1 "$REPO_ROOT/.agents/a2a/default_identity" | tr -d '[:space:]') || A2A_ID=""
fi
export A2A_ID
