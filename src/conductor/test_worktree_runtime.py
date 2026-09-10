"""Regression coverage for safe post-hardlink virtualenv launcher relocation."""

from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest

from conductor.worktree_runtime import WorktreeRuntimeError, relocate_console_scripts


def _venvs(tmp_path: Path) -> tuple[Path, Path]:
    source = tmp_path / "source-venv"
    destination = tmp_path / "destination-venv"
    for venv in (source, destination):
        (venv / "bin").mkdir(parents=True)
        (venv / "bin" / "python3").write_text("interpreter placeholder\n")
    return source, destination


def _hardlink_script(
    source: Path, destination: Path, name: str, payload: bytes
) -> tuple[Path, Path]:
    source_entry = source / "bin" / name
    destination_entry = destination / "bin" / name
    source_entry.write_bytes(payload)
    source_entry.chmod(0o751)
    os.link(source_entry, destination_entry)
    return source_entry, destination_entry


def _assert_private_entrypoint_and_destination_mode(
    tmp_path: Path,
):
    source, destination = _venvs(tmp_path)
    original = f"#!{source}/bin/python3\nfrom package import main\n".encode()
    source_entry, destination_entry = _hardlink_script(
        source, destination, "pytest", original
    )
    source_inode = source_entry.stat().st_ino

    relocated = relocate_console_scripts(source, destination)

    assert relocated == [destination_entry]
    assert source_entry.stat().st_ino == source_inode
    assert source_entry.read_bytes() == original
    assert destination_entry.stat().st_ino != source_inode
    assert destination_entry.read_bytes() == (
        f"#!{destination}/bin/python3\nfrom package import main\n".encode()
    )
    assert stat.S_IMODE(destination_entry.stat().st_mode) == 0o751

    mode_source = source / "bin" / "mode-check"
    mode_destination = destination / "bin" / "mode-check"
    mode_source.write_bytes(original)
    mode_source.chmod(0o751)
    mode_destination.write_bytes(original)
    mode_destination.chmod(0o711)
    relocate_console_scripts(source, destination)
    assert stat.S_IMODE(mode_destination.stat().st_mode) == 0o711


def test_relocation_is_idempotent_and_leaves_non_python_or_nonregular_entries_linked(
    tmp_path: Path,
):
    _assert_private_entrypoint_and_destination_mode(tmp_path / "private-mode")
    _assert_skipped_entries_continue(tmp_path / "skipped-first")
    source, destination = _venvs(tmp_path)
    original = f"#!{source}/bin/python3\nrun()\n".encode()
    _source_pytest, destination_pytest = _hardlink_script(
        source, destination, "pytest", original
    )
    foreign_source, foreign_destination = _hardlink_script(
        source, destination, "foreign", b"#!/usr/bin/env python3\nrun()\n"
    )
    binary_source, binary_destination = _hardlink_script(
        source, destination, "binary", b"\x7fELF\0not a script"
    )
    (source / "bin" / "linked").symlink_to("pytest")
    (destination / "bin" / "linked").symlink_to("pytest")

    source_link = source / "bin" / "source-link"
    source_link.symlink_to("python3")
    (destination / "bin" / "source-link").write_bytes(original)
    (source / "bin" / "destination-link").write_bytes(original)
    (destination / "bin" / "destination-link").symlink_to("python3")
    (source / "bin" / "source-directory").mkdir()
    (destination / "bin" / "source-directory").write_bytes(original)
    (source / "bin" / "destination-directory").write_bytes(original)
    (destination / "bin" / "destination-directory").mkdir()

    assert relocate_console_scripts(source, destination) == [destination_pytest]
    first_payload = destination_pytest.read_bytes()
    assert relocate_console_scripts(source, destination) == []
    assert destination_pytest.read_bytes() == first_payload
    assert foreign_source.stat().st_ino == foreign_destination.stat().st_ino
    assert binary_source.stat().st_ino == binary_destination.stat().st_ino
    assert (destination / "bin" / "linked").is_symlink()
    assert (destination / "bin" / "source-link").read_bytes() == original
    assert (destination / "bin" / "destination-link").is_symlink()
    assert (destination / "bin" / "source-directory").read_bytes() == original
    assert (destination / "bin" / "destination-directory").is_dir()


def _assert_skipped_entries_continue(tmp_path: Path):
    source, destination = _venvs(tmp_path)
    payload = f"#!{source}/bin/python3\nrun()\n".encode()
    (source / "bin" / "source-link").symlink_to("python3")
    (destination / "bin" / "source-link").write_bytes(payload)
    (source / "bin" / "destination-link").write_bytes(payload)
    (destination / "bin" / "destination-link").symlink_to("python3")
    (source / "bin" / "source-directory").mkdir()
    (destination / "bin" / "source-directory").write_bytes(payload)
    (source / "bin" / "destination-directory").write_bytes(payload)
    (destination / "bin" / "destination-directory").mkdir()
    _source_pytest, destination_pytest = _hardlink_script(
        source, destination, "pytest", payload
    )

    assert relocate_console_scripts(source, destination) == [destination_pytest]
    assert destination_pytest.read_bytes().startswith(
        f"#!{destination}/bin/python3\n".encode()
    )


def test_relocation_refuses_the_same_or_incomplete_venv(tmp_path: Path):
    source, destination = _venvs(tmp_path)

    with pytest.raises(WorktreeRuntimeError, match="must differ"):
        relocate_console_scripts(source, source)
    (destination / "bin" / "python3").unlink()
    (destination / "bin").rmdir()
    with pytest.raises(WorktreeRuntimeError, match="bin directory"):
        relocate_console_scripts(source, destination)


def test_relocation_refuses_a_destination_bin_symlink_to_the_source(tmp_path: Path):
    source, destination = _venvs(tmp_path)
    source_entry = source / "bin" / "pytest"
    source_entry.write_text(f"#!{source}/bin/python3\nrun()\n")
    source_inode = source_entry.stat().st_ino
    (destination / "bin" / "python3").unlink()
    (destination / "bin").rmdir()
    (destination / "bin").symlink_to(source / "bin", target_is_directory=True)

    with pytest.raises(WorktreeRuntimeError, match="must not be symlinks"):
        relocate_console_scripts(source, destination)

    assert source_entry.stat().st_ino == source_inode
    assert source_entry.read_text() == f"#!{source}/bin/python3\nrun()\n"
