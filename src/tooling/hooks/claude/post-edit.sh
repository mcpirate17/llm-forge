#!/usr/bin/env bash
# PostToolUse: Auto-format + structural audit on edited files.
# Auto-fixes formatting (deterministic). Reports structural issues (advisory).

set -euo pipefail

INPUT=$(cat)
FILE_PATH=$(echo "$INPUT" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('tool_input',{}).get('file_path',''))" 2>/dev/null || true)

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}'
    exit 0
fi

# ── Phase 1: Auto-format (deterministic, silent) ──────────────────────
case "$FILE_PATH" in
    *.py)
        ruff check --fix --quiet "$FILE_PATH" 2>/dev/null || true
        ruff format --quiet "$FILE_PATH" 2>/dev/null || true
        ;;
    *.rs)
        rustfmt --edition 2021 --quiet "$FILE_PATH" 2>/dev/null || true
        ;;
esac

# ── Phase 2: Structural audit (advisory) ──────────────────────────────
python3 - "$FILE_PATH" <<'PYCHECK'
import ast, json, re, sys

fp = sys.argv[1]
warnings = []

try:
    with open(fp) as f:
        content = f.read()
    lines = content.splitlines()
    n = len(lines)

    # God file (threshold mirrors CLAUDE.md "Code Rules"). Prose and append-only
    # logs are exempt -- CLAUDE.md's rule is about code, and nagging "split this
    # file" at every append to .current_work.md is pure noise in autonomous runs.
    PROSE = (".md", ".txt", ".rst", ".jsonl", ".csv", ".json")
    if n > 1250 and not fp.endswith(PROSE):
        warnings.append(f"{fp}: {n} lines. Split this file.")

    if fp.endswith(".py"):
        try:
            tree = ast.parse(content)
            for node in ast.walk(tree):
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    length = node.end_lineno - node.lineno + 1
                    if length > 100:
                        warnings.append(
                            f"{node.name}() is {length} lines at line {node.lineno}. Break it up."
                        )
        except SyntaxError:
            pass

        # Commented-out code (heuristic: lines that look like disabled statements)
        commented = sum(
            1 for line in lines
            if re.match(r'^\s*#\s*(def |class |import |from |return |raise |for |while )', line)
        )
        if commented > 2:
            warnings.append(f"{commented} lines of commented-out code. Delete them.")

        # Bare except
        bare = sum(1 for line in lines if re.match(r'^\s*except\s*:', line))
        if bare > 0:
            warnings.append(f"{bare} bare except clause(s). Catch specific exceptions.")

except Exception:
    pass

if warnings:
    msg = "POST-EDIT AUDIT: " + " | ".join(warnings)
    out = {"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": msg}}
else:
    out = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}

print(json.dumps(out))
PYCHECK
