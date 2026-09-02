"""The hook table: every hook the dispatcher runs, in settings.json order.

Each ``HookSpec`` mirrors one entry of the pre-dispatcher ``.claude/settings.json``
(``legacy_command`` is that entry verbatim, so the doctor can resolve either wiring
to a registered hook). ``adapter`` names an in-process adapter in ``adapters.py``;
an empty adapter means the body still runs as a subprocess (``argv`` relative to the
project root, under the running interpreter for ``.py`` bodies).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Final

EVENTS: Final[tuple[str, ...]] = (
    "PreToolUse",
    "PostToolUse",
    "SessionStart",
    "SessionEnd",
)
LAUNCHER: Final[str] = ".claude/hooks/dispatch.py"


@dataclass(frozen=True)
class HookSpec:
    name: str
    event: str
    matcher: str
    timeout: int
    legacy_command: str
    adapter: str = ""
    argv: tuple[str, ...] = ()
    fail_closed: bool = False
    emits: bool = True

    def matches(self, subject: str) -> bool:
        """settings.json matcher semantics: empty or ``.*`` is everything."""
        if self.matcher in ("", ".*"):
            return True
        return re.fullmatch(self.matcher, subject) is not None


_OWNER_ENV = 'env GOVERNANCE_OWNER="${GOVERNANCE_OWNER:-claude}" '
_CRG_GATE = "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py"
_PRE_EDIT = "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-edit.sh"

HOOKS: Final[tuple[HookSpec, ...]] = (
    # ── PreToolUse ────────────────────────────────────────────────────────
    HookSpec(
        "crg_gate_mark",
        "PreToolUse",
        "mcp__code[-_]review[-_]graph__.*",
        5,
        f"{_CRG_GATE} mark",
        adapter="crg_gate_mark",
        emits=False,
    ),
    HookSpec(
        "crg_gate_verify_bash",
        "PreToolUse",
        "Bash",
        5,
        f"{_OWNER_ENV}{_CRG_GATE} verify-bash",
        adapter="crg_gate_verify_bash",
        emits=False,
    ),
    HookSpec(
        "pre_bash",
        "PreToolUse",
        "Bash",
        5,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-bash.sh",
        adapter="pre_bash",
        fail_closed=True,
    ),
    HookSpec(
        "current_work_guard_bash",
        "PreToolUse",
        "Bash",
        5,
        _PRE_EDIT,
        adapter="current_work_guard",
        emits=False,
    ),
    HookSpec(
        "current_work_guard_read",
        "PreToolUse",
        "Read",
        5,
        _PRE_EDIT,
        adapter="current_work_guard",
        emits=False,
    ),
    HookSpec(
        "pre_read_skeleton",
        "PreToolUse",
        "Read",
        5,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-read-skeleton.sh",
        adapter="pre_read_skeleton",
    ),
    HookSpec(
        "crg_gate_verify",
        "PreToolUse",
        "Edit|Write|NotebookEdit",
        5,
        f"{_OWNER_ENV}{_CRG_GATE} verify",
        adapter="crg_gate_verify",
        fail_closed=True,
        emits=False,
    ),
    HookSpec(
        "current_work_guard_edit",
        "PreToolUse",
        "Edit|Write|NotebookEdit",
        5,
        _PRE_EDIT,
        adapter="current_work_guard",
        emits=False,
    ),
    # ── PostToolUse ───────────────────────────────────────────────────────
    HookSpec(
        "crg_graph_refresh",
        "PostToolUse",
        "Edit|Write|NotebookEdit",
        30,
        "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_graph_refresh.py",
        argv=("tooling/hooks/agent/crg_graph_refresh.py",),
    ),
    HookSpec(
        "post_edit",
        "PostToolUse",
        "Edit|Write",
        15,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/post-edit.sh",
        adapter="post_edit",
    ),
    HookSpec(
        "read_budget",
        "PostToolUse",
        "Read",
        5,
        "$CLAUDE_PROJECT_DIR/.agent_hooks/read_budget.py",
        adapter="read_budget",
    ),
    HookSpec(
        "obsidian_post_edit",
        "PostToolUse",
        "Edit|Write",
        5,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py post-edit",
        adapter="obsidian_post_edit",
    ),
    HookSpec(
        "post_bash_graph",
        "PostToolUse",
        "Bash",
        60,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-graph.sh",
        adapter="post_bash_graph",
    ),
    HookSpec(
        "post_bash_quiet",
        "PostToolUse",
        "Bash",
        10,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-quiet.sh",
        adapter="post_bash_quiet",
    ),
    HookSpec(
        "context_telemetry",
        "PostToolUse",
        ".*",
        5,
        "python3 $CLAUDE_PROJECT_DIR/conductor/context_telemetry.py",
        adapter="context_telemetry",
        emits=False,
    ),
    # ── SessionEnd ────────────────────────────────────────────────────────
    HookSpec(
        "obsidian_session_end",
        "SessionEnd",
        "",
        10,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py session-end",
        adapter="obsidian_session_end",
        emits=False,
    ),
    # ── SessionStart ──────────────────────────────────────────────────────
    HookSpec(
        "session_start",
        "SessionStart",
        "",
        10,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/session-start.sh",
        argv=("tooling/hooks/claude/session-start.sh",),
    ),
    HookSpec(
        "session_handoff",
        "SessionStart",
        "",
        10,
        "$CLAUDE_PROJECT_DIR/.claude/hooks/session-handoff.sh",
        argv=("tooling/hooks/claude/session-handoff.sh",),
    ),
)


def hooks_for(event: str) -> tuple[HookSpec, ...]:
    return tuple(spec for spec in HOOKS if spec.event == event)


def dispatcher_command(event: str) -> str:
    return f"$CLAUDE_PROJECT_DIR/{LAUNCHER} {event}"


def event_timeout(event: str) -> int:
    """Outer timeout the harness applies: the slowest hook plus one second."""
    return max(spec.timeout for spec in hooks_for(event)) + 1


def settings_block() -> dict:
    """The ``hooks`` block wiring every event to the dispatcher."""
    return {
        "hooks": {
            event: [
                {
                    "matcher": "" if event.startswith("Session") else ".*",
                    "hooks": [
                        {
                            "type": "command",
                            "command": dispatcher_command(event),
                            "timeout": event_timeout(event),
                        }
                    ],
                }
            ]
            for event in EVENTS
        }
    }


def resolve_legacy(command: str) -> HookSpec | None:
    """The registered hook a pre-dispatcher settings.json command names."""
    for spec in HOOKS:
        if spec.legacy_command == command:
            return spec
    return None


def resolve_dispatcher(command: str) -> str | None:
    """The event a dispatcher settings.json command names."""
    for event in EVENTS:
        if command == dispatcher_command(event):
            return event
    return None
