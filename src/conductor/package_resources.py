"""Read verified resources from the installed ``conductor-tooling`` wheel."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import stat
import unicodedata
from dataclasses import dataclass
from importlib import import_module, metadata, resources
from importlib.machinery import NamespaceLoader, SourceFileLoader
from pathlib import Path, PurePosixPath
from typing import Literal, cast

from conductor.project_context import ContextError, ErrorDetail

_DISTRIBUTION_NAME = "conductor-tooling"
_MAX_RESOURCE_BYTES = 4 * 1024 * 1024
_PACKAGE = Literal["conductor", "tooling"]


@dataclass(frozen=True, slots=True)
class PackageResource:
    """A byte-verified asset owned by the installed tooling distribution."""

    package: _PACKAGE
    name: str
    distribution_version: str
    data: bytes
    sha256: str


def _fail(code: str, field: str | None, message: str) -> None:
    """Raise the shared, bounded contract error."""

    raise ContextError(ErrorDetail(code=code, field=field, message=message[:1024]))


def _normalized_distribution_name(value: str) -> str:
    return re.sub(r"[-_.]+", "-", value).lower()


def _validated_name(name: str) -> tuple[str, ...]:
    if not isinstance(name, str) or not name:
        _fail(
            "RESOURCE_NAME_INVALID", "name", "resource name must be a non-empty string"
        )
    if "\\" in name or PurePosixPath(name).is_absolute():
        _fail(
            "RESOURCE_NAME_INVALID",
            "name",
            "resource name must be relative POSIX components",
        )
    components = tuple(name.split("/"))
    if any(
        not component
        or component in {".", ".."}
        or any(unicodedata.category(character) == "Cc" for character in component)
        for component in components
    ):
        _fail("RESOURCE_NAME_INVALID", "name", "resource name has an unsafe component")
    return components


def _distribution() -> metadata.Distribution:
    matches: list[metadata.Distribution] = []
    try:
        candidates = metadata.distributions()
        for candidate in candidates:
            candidate_name = candidate.metadata.get("Name")
            if (
                candidate_name
                and _normalized_distribution_name(candidate_name) == _DISTRIBUTION_NAME
            ):
                matches.append(candidate)
    except (AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"cannot inspect installed conductor-tooling metadata: {exc}",
        )
    if len(matches) != 1:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "expected exactly one installed conductor-tooling distribution",
        )
    return matches[0]


def _canonical_root(distribution: metadata.Distribution, package: _PACKAGE) -> Path:
    try:
        recorded_files = distribution.files
    except (AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"cannot read installed conductor-tooling RECORD metadata: {exc}",
        )
    if not recorded_files:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "installed conductor-tooling distribution has no RECORD metadata",
        )
    prefix = f"{package}/"
    if not any(str(record).startswith(prefix) for record in recorded_files):
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "package",
            f"installed conductor-tooling RECORD does not own the {package!r} package",
        )
    root = Path(distribution.locate_file(package))
    try:
        if root.is_symlink() or not root.is_dir():
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                f"installed {package!r} package root is not an unpacked directory",
            )
        return root.resolve(strict=True)
    except OSError as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "package",
            f"cannot resolve installed {package!r} package root: {exc}",
        )
    raise AssertionError("unreachable")


def _reject_editable(distribution: metadata.Distribution) -> None:
    try:
        direct_url = distribution.read_text("direct_url.json")
    except (AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"cannot inspect conductor-tooling direct_url metadata: {exc}",
        )
    if direct_url is None:
        return
    try:
        parsed = json.loads(direct_url)
    except (TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"conductor-tooling direct_url metadata is malformed: {exc}",
        )
    if not isinstance(parsed, dict):
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "direct_url metadata must be an object",
        )
    directory_info = parsed.get("dir_info")
    if directory_info is None:
        return
    if not isinstance(directory_info, dict) or not isinstance(
        directory_info.get("editable", False), bool
    ):
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "direct_url dir_info is malformed",
        )
    if directory_info.get("editable"):
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "editable installations are unsupported",
        )


def _import_anchor(package: _PACKAGE, root: Path) -> None:
    try:
        module = import_module(package)
        specification = module.__spec__
        locations = specification.submodule_search_locations if specification else None
        if locations is None:
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                f"installed {package!r} has no package search location",
            )
        resolved_locations = tuple(
            Path(location).resolve(strict=True) for location in locations
        )
        if (
            len(resolved_locations) != 1
            or resolved_locations[0] != root
            or not isinstance(specification.loader, (NamespaceLoader, SourceFileLoader))
        ):
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                f"import origin for {package!r} does not match its installed distribution root",
            )
        if isinstance(specification.loader, SourceFileLoader):
            module_file = getattr(module, "__file__", None)
            if (
                not isinstance(module_file, str)
                or not isinstance(specification.origin, str)
                or Path(module_file).resolve(strict=True).parent != root
                or Path(specification.origin).resolve(strict=True).parent != root
            ):
                _fail(
                    "RESOURCE_LAYOUT_UNSUPPORTED",
                    "package",
                    f"module origin for {package!r} is not its installed filesystem root",
                )
        elif (
            specification.origin is not None
            or getattr(module, "__file__", None) is not None
        ):
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                f"namespace origin for {package!r} is not its installed filesystem root",
            )
        resource_root = resources.files(package)
        if isinstance(specification.loader, SourceFileLoader) and (
            not isinstance(resource_root, Path)
            or resource_root.resolve(strict=True) != root
        ):
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                f"resource backend for {package!r} is not its installed filesystem root",
            )
    except ContextError:
        raise
    except (ImportError, AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "package",
            f"cannot anchor {package!r} to its installed distribution root: {exc}",
        )


def _record_for(
    distribution: metadata.Distribution, package: _PACKAGE, name: str
) -> metadata.PackagePath:
    expected = f"{package}/{name}"
    try:
        records = [
            record for record in distribution.files or () if str(record) == expected
        ]
    except (AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"cannot read installed conductor-tooling RECORD metadata: {exc}",
        )
    if not records:
        _fail(
            "RESOURCE_NOT_FOUND",
            "name",
            "resource is not owned by conductor-tooling RECORD",
        )
    if len(records) != 1 or records[0].hash is None or records[0].size is None:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "name",
            "resource RECORD entry must have one sha256 digest and size",
        )
    record = records[0]
    try:
        hash_mode = record.hash.mode
        record_size = int(record.size)
    except (AttributeError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "name",
            f"resource RECORD entry is malformed: {exc}",
        )
    if hash_mode != "sha256":
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "name",
            "resource RECORD digest must use sha256",
        )
    if record_size < 0:
        _fail("RESOURCE_LAYOUT_UNSUPPORTED", "name", "resource RECORD size is invalid")
    return record


def _identity(observed: os.stat_result) -> tuple[int, int, int, int]:
    return observed.st_dev, observed.st_ino, observed.st_size, observed.st_mtime_ns


def _close_directories(
    directories: list[tuple[int, Path, tuple[int, int, int, int]]],
) -> None:
    for directory_descriptor, _, _ in reversed(directories):
        os.close(directory_descriptor)


def _adopt_directory(
    directories: list[tuple[int, Path, tuple[int, int, int, int]]],
    target: str | Path,
    current: Path,
    flags: int,
    *,
    dir_fd: int | None = None,
) -> os.stat_result:
    """Open `target`, hand the descriptor to `directories`, and return its stat.

    Ownership transfers inside the try, so a caller that raises afterwards --
    `_fail` on a non-directory included -- still releases the descriptor via
    `_close_directories`. Anything that raises before the handover leaves
    `adopted` false and the finally closes the descriptor here.
    """
    descriptor = os.open(target, flags, dir_fd=dir_fd)
    adopted = False
    try:
        observed = os.fstat(descriptor)
        directories.append((descriptor, current, _identity(observed)))
        adopted = True
    finally:
        if not adopted:
            os.close(descriptor)
    return observed


def _open_anchored_asset(
    root: Path, components: tuple[str, ...]
) -> tuple[Path, int, list[tuple[int, Path, tuple[int, int, int, int]]]]:
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    directories: list[tuple[int, Path, tuple[int, int, int, int]]] = []
    current = root
    try:
        root_stat = _adopt_directory(directories, root, current, directory_flags)
        if not stat.S_ISDIR(root_stat.st_mode):
            _fail(
                "RESOURCE_LAYOUT_UNSUPPORTED",
                "package",
                "package root is not a directory",
            )
        for component in components[:-1]:
            current = current / component
            child_stat = _adopt_directory(
                directories,
                component,
                current,
                directory_flags,
                dir_fd=directories[-1][0],
            )
            if not stat.S_ISDIR(child_stat.st_mode):
                _fail(
                    "RESOURCE_UNSAFE", "name", "resource has a non-directory ancestor"
                )
        asset = current / components[-1]
        descriptor = os.open(
            components[-1],
            os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=directories[-1][0],
        )
    except BaseException:
        _close_directories(directories)
        raise
    return asset, descriptor, directories


def _read_verified(
    root: Path, components: tuple[str, ...], record: metadata.PackagePath
) -> bytes:
    try:
        expected_size = int(cast(str, record.size))
    except (TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "name",
            f"resource RECORD size is invalid: {exc}",
        )
    if expected_size > _MAX_RESOURCE_BYTES:
        _fail(
            "RESOURCE_TOO_LARGE", "name", "resource RECORD size exceeds the 4 MiB limit"
        )
    descriptor = -1
    directories: list[tuple[int, Path, tuple[int, int, int, int]]] = []
    try:
        asset, descriptor, directories = _open_anchored_asset(root, components)
        with os.fdopen(descriptor, "rb") as resource_file:
            descriptor = -1
            opened = resource_file.fileno()
            opened_stat = os.fstat(opened)
            if not stat.S_ISREG(opened_stat.st_mode):
                _fail(
                    "RESOURCE_UNSAFE",
                    "name",
                    "resource descriptor is not a regular file",
                )
            if opened_stat.st_size > _MAX_RESOURCE_BYTES:
                _fail("RESOURCE_TOO_LARGE", "name", "resource exceeds the 4 MiB limit")
            if opened_stat.st_size != expected_size:
                _fail(
                    "RESOURCE_CHANGED",
                    "name",
                    "resource size differs from its RECORD entry",
                )
            data = resource_file.read(expected_size)
            after = os.fstat(opened)
    except ContextError:
        raise
    except FileNotFoundError:
        _fail("RESOURCE_NOT_FOUND", "name", "resource asset is absent")
    except OSError as exc:
        _fail("RESOURCE_UNSAFE", "name", f"cannot read resource asset: {exc}")
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        _close_directories(directories)
    if len(data) != expected_size or _identity(opened_stat) != _identity(after):
        _fail(
            "RESOURCE_CHANGED", "name", "resource descriptor changed while it was read"
        )
    try:
        if _identity(asset.stat(follow_symlinks=False)) != _identity(opened_stat):
            _fail("RESOURCE_CHANGED", "name", "resource path changed while it was read")
        for _, directory_path, directory_identity in directories:
            if (
                _identity(directory_path.stat(follow_symlinks=False))
                != directory_identity
            ):
                _fail(
                    "RESOURCE_CHANGED",
                    "name",
                    "resource ancestor changed while it was read",
                )
    except FileNotFoundError:
        _fail("RESOURCE_CHANGED", "name", "resource path disappeared while it was read")
    except OSError as exc:
        _fail("RESOURCE_CHANGED", "name", f"cannot revalidate resource path: {exc}")
    digest = hashlib.sha256(data).digest()
    record_hash = cast(metadata.FileHash, record.hash)
    encoded_digest = base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")
    if encoded_digest != record_hash.value:
        _fail(
            "RESOURCE_CHANGED", "name", "resource bytes do not match its RECORD digest"
        )
    return data


def read_package_resource(package: _PACKAGE, name: str) -> PackageResource:
    """Return one bounded, RECORD-verified resource from ``conductor-tooling``."""

    if not isinstance(package, str) or package not in {"conductor", "tooling"}:
        _fail(
            "RESOURCE_NAME_INVALID",
            "package",
            "package must be 'conductor' or 'tooling'",
        )
    components = _validated_name(name)
    selected_package = cast(_PACKAGE, package)
    distribution = _distribution()
    _reject_editable(distribution)
    root = _canonical_root(distribution, selected_package)
    _import_anchor(selected_package, root)
    record = _record_for(distribution, selected_package, name)
    data = _read_verified(root, components, record)
    try:
        distribution_version = distribution.version
    except (AttributeError, OSError, TypeError, ValueError) as exc:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            f"installed conductor-tooling version metadata is invalid: {exc}",
        )
    if not isinstance(distribution_version, str) or not distribution_version:
        _fail(
            "RESOURCE_LAYOUT_UNSUPPORTED",
            "distribution",
            "installed conductor-tooling has no version",
        )
    return PackageResource(
        package=selected_package,
        name=name,
        distribution_version=distribution_version,
        data=data,
        sha256=hashlib.sha256(data).hexdigest(),
    )


__all__ = ["PackageResource", "read_package_resource"]
