#!/usr/bin/env python3
"""Install portable A2A startup hooks without replacing provider settings.

The default is a read-only plan.  ``--apply`` is required for install,
uninstall, backup, or rollback writes.  Each applied change stores the exact
previous file state beside the provider config so rollback is lossless.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import sys
import tempfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

from conductor.project_paths import host_root
ROOT: Final[Path] = host_root()
BACKUP_SUFFIX: Final[str] = ".a2a-session-start.bak"
BACKUP_SCHEMA_VERSION: Final[int] = 1
MANAGED_MODULE: Final[str] = "conductor.a2a_session_start"
MANAGED_NAME: Final[str] = "a2a-session-start"


class HookInstallerError(RuntimeError):
    """A provider config cannot be safely planned or updated."""


@dataclass(frozen=True)
class ProviderSpec:
    """Provider config location and supported context-producing hook event."""

    name: str
    relative_path: Path
    event: str
    timeout: int
    include_matcher: bool
    once_per_session: bool = False


PROVIDERS: Final[dict[str, ProviderSpec]] = {
    "codex": ProviderSpec("codex", Path(".codex/hooks.json"), "SessionStart", 15, True),
    "claude": ProviderSpec(
        "claude", Path(".claude/settings.json"), "SessionStart", 15, True
    ),
    "qwen": ProviderSpec(
        "qwen", Path(".qwen/settings.json"), "SessionStart", 15_000, False
    ),
    # Grok discards SessionStart stdout.  UserPromptSubmit is the first-turn
    # context path; --once-per-session keeps subsequent prompts silent.
    "grok": ProviderSpec(
        "grok",
        Path(".grok/hooks/workspace.json"),
        "UserPromptSubmit",
        15,
        False,
        once_per_session=True,
    ),
}


@dataclass(frozen=True)
class FileState:
    """Exact state of a provider config before or after an operation."""

    exists: bool
    text: str


@dataclass(frozen=True)
class ConfigPlan:
    """One provider's dry-run or apply plan."""

    provider: str
    path: Path
    before: FileState
    after: FileState

    @property
    def changed(self) -> bool:
        return self.before != self.after


def startup_command(
    spec: ProviderSpec,
    *,
    interpreter: str = sys.executable,
    identity: str | None = None,
    state_dir: Path | None = None,
) -> str:
    """Build the exact portable command owned by this installer."""

    command = [
        interpreter,
        "-m",
        MANAGED_MODULE,
        "--provider",
        spec.name,
        "--output",
        "hook-json",
        "--max-messages",
        "8",
        "--preview-chars",
        "140",
        "--max-chars",
        "1200",
    ]
    if identity is not None:
        if not identity.strip():
            raise HookInstallerError("--identity cannot be blank")
        command.extend(("--identity", identity))
    if state_dir is not None:
        command.extend(("--state-dir", str(state_dir)))
    if spec.once_per_session:
        command.append("--once-per-session")
    from conductor._native import hook_installer_shlex_join_native

    return hook_installer_shlex_join_native(command)


def _without_managed_hooks(config: dict[str, Any]) -> dict[str, Any]:
    from conductor._native import hook_installer_without_managed_native

    try:
        updated = json.loads(
            hook_installer_without_managed_native(json.dumps(config), MANAGED_MODULE)
        )
    except ValueError as exc:
        raise HookInstallerError(str(exc)) from exc
    return updated


def merge_install(
    config: Mapping[str, Any], spec: ProviderSpec, command: str
) -> dict[str, Any]:
    """Return a config containing exactly one managed hook for ``spec``."""

    from conductor._native import hook_installer_merge_install_native

    if not isinstance(config, dict):
        raise HookInstallerError("provider config root must be a JSON object")
    spec_facts = {
        "event": spec.event,
        "timeout": spec.timeout,
        "include_matcher": spec.include_matcher,
        "install_hook_name": spec.name in ("qwen", "grok"),
    }
    try:
        updated = json.loads(
            hook_installer_merge_install_native(
                json.dumps(dict(config)),
                json.dumps(spec_facts),
                command,
                MANAGED_NAME,
                MANAGED_MODULE,
            )
        )
    except ValueError as exc:
        raise HookInstallerError(str(exc)) from exc
    return updated


def merge_uninstall(config: Mapping[str, Any]) -> dict[str, Any]:
    """Remove only commands owned by this installer."""

    if not isinstance(config, dict):
        raise HookInstallerError("provider config root must be a JSON object")
    return _without_managed_hooks(dict(config))


def _read_state(path: Path) -> FileState:
    try:
        return FileState(True, path.read_text())
    except FileNotFoundError:
        return FileState(False, "")


def _parse_config(path: Path, state: FileState) -> dict[str, Any]:
    if not state.exists:
        return {}
    try:
        payload = json.loads(state.text)
    except json.JSONDecodeError as exc:
        raise HookInstallerError(f"invalid JSON in {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise HookInstallerError(f"provider config {path} must contain a JSON object")
    return payload


def _render_config(payload: Mapping[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def build_plan(
    *,
    action: str,
    root: Path,
    providers: Sequence[str],
    interpreter: str = sys.executable,
    identity: str | None = None,
    state_dir: Path | None = None,
) -> list[ConfigPlan]:
    """Build install or uninstall plans without writing the filesystem."""

    if action not in ("install", "uninstall"):
        raise HookInstallerError(f"cannot build config plan for action {action!r}")
    plans: list[ConfigPlan] = []
    for provider in providers:
        spec = PROVIDERS[provider]
        path = root / spec.relative_path
        before = _read_state(path)
        config = _parse_config(path, before)
        if action == "install":
            command = startup_command(
                spec,
                interpreter=interpreter,
                identity=identity,
                state_dir=state_dir,
            )
            updated = merge_install(config, spec, command)
        else:
            updated = merge_uninstall(config)
        after = FileState(before.exists or bool(updated), _render_config(updated))
        if not before.exists and not updated:
            after = FileState(False, "")
        plans.append(ConfigPlan(provider, path, before, after))
    return plans


def backup_path(path: Path) -> Path:
    """Return the reversible backup path for a provider config."""

    return path.with_name(path.name + BACKUP_SUFFIX)


def _backup_payload(path: Path, state: FileState) -> str:
    return (
        json.dumps(
            {
                "schema_version": BACKUP_SCHEMA_VERSION,
                "target": str(path.resolve()),
                "existed": state.exists,
                "content": state.text,
            },
            ensure_ascii=False,
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )


def _read_backup(path: Path) -> FileState:
    saved = backup_path(path)
    try:
        payload = json.loads(saved.read_text())
    except FileNotFoundError as exc:
        raise HookInstallerError(f"backup missing for {path}: {saved}") from exc
    except json.JSONDecodeError as exc:
        raise HookInstallerError(f"invalid backup JSON in {saved}: {exc}") from exc
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != BACKUP_SCHEMA_VERSION
        or payload.get("target") != str(path.resolve())
        or not isinstance(payload.get("existed"), bool)
        or not isinstance(payload.get("content"), str)
    ):
        raise HookInstallerError(f"backup contract mismatch in {saved}")
    return FileState(payload["existed"], payload["content"])


def _atomic_write(path: Path, text: str, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        with os.fdopen(descriptor, "w") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary_path, mode)
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


def _restore_state(path: Path, state: FileState) -> None:
    if state.exists:
        mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o600
        _atomic_write(path, state.text, mode=mode)
    else:
        path.unlink(missing_ok=True)


def apply_plans(plans: Sequence[ConfigPlan]) -> None:
    """Apply validated plans, backing up every changed target first."""

    changed = [plan for plan in plans if plan.changed]
    completed: list[tuple[ConfigPlan, FileState]] = []
    try:
        for plan in changed:
            saved_path = backup_path(plan.path)
            old_backup = _read_state(saved_path)
            _atomic_write(saved_path, _backup_payload(plan.path, plan.before))
            completed.append((plan, old_backup))
            _restore_state(plan.path, plan.after)
    except OSError:
        for plan, old_backup in reversed(completed):
            _restore_state(plan.path, plan.before)
            _restore_state(backup_path(plan.path), old_backup)
        raise


def backup_configs(*, root: Path, providers: Sequence[str]) -> list[ConfigPlan]:
    """Return no-target-change plans used to describe explicit backups."""

    return [
        ConfigPlan(
            provider,
            root / PROVIDERS[provider].relative_path,
            _read_state(root / PROVIDERS[provider].relative_path),
            _read_state(root / PROVIDERS[provider].relative_path),
        )
        for provider in providers
    ]


def apply_backups(plans: Sequence[ConfigPlan]) -> None:
    completed: list[tuple[Path, FileState]] = []
    try:
        for plan in plans:
            saved_path = backup_path(plan.path)
            completed.append((saved_path, _read_state(saved_path)))
            _atomic_write(saved_path, _backup_payload(plan.path, plan.before))
    except OSError:
        for saved_path, old_state in reversed(completed):
            _restore_state(saved_path, old_state)
        raise


def rollback_plans(*, root: Path, providers: Sequence[str]) -> list[ConfigPlan]:
    """Plan restoring each config's most recent reversible backup."""

    plans: list[ConfigPlan] = []
    for provider in providers:
        path = root / PROVIDERS[provider].relative_path
        plans.append(ConfigPlan(provider, path, _read_state(path), _read_backup(path)))
    return plans


def apply_rollbacks(plans: Sequence[ConfigPlan]) -> None:
    """Swap targets and backups, making rollback itself reversible."""

    completed: list[tuple[ConfigPlan, FileState]] = []
    try:
        for plan in plans:
            saved_path = backup_path(plan.path)
            completed.append((plan, _read_state(saved_path)))
            _restore_state(plan.path, plan.after)
            _atomic_write(saved_path, _backup_payload(plan.path, plan.before))
    except OSError:
        for plan, old_backup in reversed(completed):
            _restore_state(plan.path, plan.before)
            _restore_state(backup_path(plan.path), old_backup)
        raise


def _selected_providers(values: Sequence[str] | None) -> list[str]:
    if not values or "all" in values:
        return sorted(PROVIDERS)
    return list(dict.fromkeys(values))


def _summary(action: str, plans: Sequence[ConfigPlan], applied: bool) -> dict[str, Any]:
    return {
        "action": action,
        "applied": applied,
        "providers": [
            {
                "provider": plan.provider,
                "path": str(plan.path),
                "changed": plan.changed if action != "backup" else False,
                "backup": str(backup_path(plan.path)),
            }
            for plan in plans
        ],
    }


def build_parser() -> argparse.ArgumentParser:
    """Build the dry-run-by-default installer CLI."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        nargs="?",
        choices=("backup", "install", "rollback", "uninstall"),
        default="install",
    )
    parser.add_argument(
        "--provider",
        action="append",
        choices=("all", *sorted(PROVIDERS)),
        help="provider to update; repeat as needed (default: all)",
    )
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--interpreter", default=sys.executable)
    parser.add_argument(
        "--identity",
        help="embed an explicit identity; otherwise use the hook environment",
    )
    parser.add_argument(
        "--state-dir", type=Path, help="embed a non-default A2A state directory"
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write the planned operation (default is dry-run)",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Plan or apply portable provider hook changes."""

    args = build_parser().parse_args(argv)
    providers = _selected_providers(args.provider)
    try:
        if args.action in ("install", "uninstall"):
            plans = build_plan(
                action=args.action,
                root=args.root,
                providers=providers,
                interpreter=args.interpreter,
                identity=args.identity,
                state_dir=args.state_dir,
            )
            if args.apply:
                apply_plans(plans)
        elif args.action == "backup":
            plans = backup_configs(root=args.root, providers=providers)
            if args.apply:
                apply_backups(plans)
        else:
            plans = rollback_plans(root=args.root, providers=providers)
            if args.apply:
                apply_rollbacks(plans)
        print(
            json.dumps(
                _summary(args.action, plans, args.apply), indent=2, sort_keys=True
            )
        )
        return 0
    except (HookInstallerError, OSError) as exc:
        print(f"hook-installer: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
