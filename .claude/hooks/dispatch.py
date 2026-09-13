#!/usr/bin/env python3
"""Launcher only -- the body is src/tooling/hooks/dispatch (single-process hook dispatcher).

One interpreter per hook event: resolves the checkout from its own location,
puts the package root (this repository's ``src/`` tree) on ``sys.path`` and
calls the dispatcher in-process (no exec, no second interpreter). Usage from
``.claude/settings.json``:
``$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py <PreToolUse|PostToolUse|SessionStart|SessionEnd>``.
"""

import os
import sys
from pathlib import Path

# This launcher sits at <checkout>/.claude/hooks/dispatch.py; the package it
# dispatches into lives under <checkout>/src (the src layout this repository
# ships). A layout move surfaces here as a loud ImportError, not a quiet miss.
REPO_ROOT: Path = Path(__file__).resolve().parents[2]
PKG_ROOT: Path = REPO_ROOT / "src"
os.environ["PROJECT_DIR"] = str(REPO_ROOT)
sys.path.insert(0, str(PKG_ROOT))

# The shebang resolves against PATH -- whatever venv the agent activated, not this
# checkout's. `own_interpreter` decides what to switch to; `paths` is stdlib-only, so
# importing it is safe under any interpreter. Guarded by an env flag so the re-exec
# happens at most once. Nothing to switch to is reported, never swallowed.
from tooling.hooks.dispatch.paths import own_interpreter  # noqa: E402

if os.environ.get("HOOK_DISPATCH_REEXEC") != "1":
    os.environ["HOOK_DISPATCH_REEXEC"] = "1"
    _own = own_interpreter(REPO_ROOT, sys.executable)
    if _own is not None:
        _self = str(Path(__file__).resolve())
        os.execv(str(_own), [str(_own), _self, *sys.argv[1:]])
    elif (PKG_ROOT / "conductor").is_dir() and not (
        REPO_ROOT / ".venv" / "bin" / "python"
    ).is_file():
        # Only a checkout of the tooling itself builds native extensions in-tree, so
        # only there is a missing .venv worth saying anything about. A foreign project
        # scaffolded by `conductor init` runs the installed package under whatever
        # interpreter it installed into, and must not be nagged on every hook event.
        print(
            f"[dispatch] warning: {REPO_ROOT}/.venv/bin/python is absent; running "
            f"hooks under {sys.executable}, which may not carry this checkout's "
            "native extensions",
            file=sys.stderr,
        )

from tooling.hooks.dispatch.__main__ import main  # noqa: E402

raise SystemExit(main())
