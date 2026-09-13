"""Registry invariants: template in sync, every body present, every legacy command resolvable."""

from __future__ import annotations

import json
import os
from pathlib import Path

from tooling.hooks.dispatch import adapters, registry

ROOT = Path(__file__).resolve().parents[3]  # package root (src/) in this repository
REPO = Path(__file__).resolve().parents[4]  # checkout root, where .claude/ lives
TEMPLATE = ROOT / "tooling" / "hooks" / "claude" / "settings.dispatcher.json"

# The 18 commands live in the main checkout's (gitignored) .claude/settings.json on 2026-09-02.
LIVE_COMMANDS = (
    "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py mark",
    'env GOVERNANCE_OWNER="${GOVERNANCE_OWNER:-claude}" $CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py verify-bash',
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-bash.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-edit.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-read-skeleton.sh",
    'env GOVERNANCE_OWNER="${GOVERNANCE_OWNER:-claude}" $CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py verify',
    "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_graph_refresh.py",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-edit.sh",
    "$CLAUDE_PROJECT_DIR/.agent_hooks/read_budget.py",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py post-edit",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-graph.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-quiet.sh",
    "python3 $CLAUDE_PROJECT_DIR/conductor/context_telemetry.py",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py session-end",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/session-start.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/session-handoff.sh",
)


def test_settings_template_is_generated_from_the_registry():
    assert json.loads(TEMPLATE.read_text()) == registry.settings_block()


def test_settings_block_wires_every_event_once_to_the_launcher():
    block = registry.settings_block()["hooks"]
    assert tuple(block) == registry.EVENTS
    for event, groups in block.items():
        assert len(groups) == 1 and len(groups[0]["hooks"]) == 1
        hook = groups[0]["hooks"][0]
        assert hook["command"] == f"$CLAUDE_PROJECT_DIR/{registry.LAUNCHER} {event}"
        assert hook["timeout"] == max(s.timeout for s in registry.hooks_for(event)) + 1
        assert registry.resolve_dispatcher(hook["command"]) == event


def test_launcher_is_tracked_and_executable():
    launcher = REPO / registry.LAUNCHER
    assert launcher.is_file() and launcher.stat().st_size > 0
    assert os.access(launcher, os.X_OK)
    assert launcher.read_text().startswith("#!/usr/bin/env python3")


def test_every_spec_has_a_runnable_body():
    for spec in registry.HOOKS:
        assert spec.event in registry.EVENTS
        assert bool(spec.adapter) != bool(spec.argv), spec.name
        if spec.adapter:
            assert callable(getattr(adapters, spec.adapter)), spec.name
        else:
            body = ROOT / spec.argv[0]
            assert body.is_file() and body.stat().st_size > 0, spec.name
            assert os.access(body, os.X_OK) or body.suffix == ".py", spec.name


def test_names_are_unique():
    names = [s.name for s in registry.HOOKS]
    assert len(names) == len(set(names))


def test_every_live_command_resolves_to_a_registered_spec():
    for command in LIVE_COMMANDS:
        assert registry.resolve_legacy(command) is not None, command
    assert registry.resolve_legacy("$CLAUDE_PROJECT_DIR/.claude/hooks/nope.sh") is None


def test_natively_served_is_empty_by_default(monkeypatch):
    monkeypatch.delenv("FORGE_NATIVE_HOOKS", raising=False)
    assert registry.natively_served() == frozenset()


def test_natively_served_parses_and_trims_the_env_var(monkeypatch):
    monkeypatch.setenv("FORGE_NATIVE_HOOKS", " pre_bash ,, bash_write_targets")
    assert registry.natively_served() == frozenset({"pre_bash", "bash_write_targets"})


def test_matchers():
    spec = next(s for s in registry.HOOKS if s.name == "crg_gate_mark")
    assert spec.matches("mcp__code-review-graph__locate_tool")
    assert spec.matches("mcp__code_review_graph__locate_tool")
    assert not spec.matches("Bash")
    edit = next(s for s in registry.HOOKS if s.name == "crg_gate_verify")
    assert (
        edit.matches("Edit")
        and edit.matches("NotebookEdit")
        and not edit.matches("Read")
    )
    assert all(s.matches("anything") for s in registry.HOOKS if s.matcher in ("", ".*"))
