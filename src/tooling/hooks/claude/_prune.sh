# shellcheck shell=bash
# Generic mtime pruning helpers, sourced by hooks that expire scratch output.
# No project paths live here: the caller supplies dir/age/label, so a project
# extension (.claude/hooks/project/*) owns *what* is pruned and this file owns
# *how*. Output goes to stderr so a hook's JSON payload stays clean.

prune_stale_subdirs() {
  local dir="$1" days="$2" label="$3"
  [[ -d "$dir" ]] || return 0
  local pruned
  pruned=$(find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime "+$days" -print -exec rm -rf {} + 2>/dev/null | wc -l || true)
  if [[ "${pruned:-0}" -gt 0 ]]; then
    echo "[session-start] pruned $pruned dir(s) from $label older than ${days}d" >&2
  fi
}

prune_root_files() {
  local dir="$1" days="$2" label="$3" extra_exclude="${4:-}"
  [[ -d "$dir" ]] || return 0
  local pruned
  if [[ -n "$extra_exclude" ]]; then
    pruned=$(find "$dir" -maxdepth 1 -type f -mtime "+$days" ! -name "$extra_exclude" -print -delete 2>/dev/null | wc -l || true)
  else
    pruned=$(find "$dir" -maxdepth 1 -type f -mtime "+$days" -print -delete 2>/dev/null | wc -l || true)
  fi
  if [[ "${pruned:-0}" -gt 0 ]]; then
    echo "[session-start] pruned $pruned file(s) from $label older than ${days}d" >&2
  fi
}
