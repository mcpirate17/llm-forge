#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "tasks" / "notebooklm" / "codex_context_bundle.md"
INCLUDE_FILES = (
    "AGENTS.md",
    "README.md",
    "pyproject.toml",
    ".pre-commit-config.yaml",
)
MAX_README_LINES = 260
SECRETISH_RE = re.compile(
    r"(?i)\b(api[_-]?key|token|password|passwd|secret|credential)\b\s*[:=]\s*\S+"
)


def _redact(text: str) -> str:
    return SECRETISH_RE.sub(lambda m: f"{m.group(1)}=<redacted>", text)


def _git(args: list[str]) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def _read_curated(path: Path) -> str:
    text = path.read_text(encoding="utf-8", errors="replace")
    if path.name == "README.md":
        lines = text.splitlines()
        text = "\n".join(lines[:MAX_README_LINES])
        if len(lines) > MAX_README_LINES:
            text += "\n\n[README truncated for NotebookLM bundle.]"
    return _redact(text)


def _makefile_targets() -> str:
    makefile = ROOT / "Makefile"
    if not makefile.exists():
        return ""
    targets = []
    for line in makefile.read_text(encoding="utf-8", errors="replace").splitlines():
        if "##" not in line or line.startswith("\t"):
            continue
        target, desc = line.split("##", 1)
        name = target.split(":", 1)[0].strip()
        if name:
            targets.append(f"- `{name}`: {desc.strip()}")
    return "\n".join(targets)


def build_bundle() -> str:
    branch = _git(["branch", "--show-current"]) or "(unknown)"
    head = _git(["rev-parse", "--short", "HEAD"]) or "(unknown)"
    parts = [
        "# Codex Context Bundle",
        "",
        "Curated local context for NotebookLM upload. Protected research data, "
        "databases, runtime events, perf artifacts, and secrets are intentionally excluded.",
        "",
        f"- Branch: `{branch}`",
        f"- HEAD: `{head}`",
        "",
        "## Make Targets",
        _makefile_targets() or "- No documented Make targets found.",
        "",
    ]
    for rel in INCLUDE_FILES:
        path = ROOT / rel
        if not path.exists():
            continue
        parts.extend(
            [
                f"## `{rel}`",
                "",
                "```",
                _read_curated(path),
                "```",
                "",
            ]
        )
    return "\n".join(parts)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Write a curated Markdown bundle for manual NotebookLM upload."
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT,
        help="Output Markdown file.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    out = args.out if args.out.is_absolute() else ROOT / args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(build_bundle(), encoding="utf-8")
    print(out.relative_to(ROOT))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
