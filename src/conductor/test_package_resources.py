"""Installed-wheel integration tests for :mod:`conductor.package_resources`."""

from __future__ import annotations

import base64
import csv
import hashlib
import json
import shutil
import subprocess
import venv
import zipfile
from pathlib import Path

import pytest

_MAX_RESOURCE_BYTES = 4 * 1024 * 1024
_ROOT = Path(__file__).resolve().parents[1]


def _record_digest(data: bytes) -> str:
    return (
        base64.urlsafe_b64encode(hashlib.sha256(data).digest())
        .decode("ascii")
        .rstrip("=")
    )


def _write_wheel(wheel: Path) -> None:
    """Build a tiny current-source wheel with a complete installed RECORD."""

    files = {
        "conductor/__init__.py": b"",
        "conductor/project_context.py": (
            _ROOT / "conductor/project_context.py"
        ).read_bytes(),
        "conductor/package_resources.py": (
            _ROOT / "conductor/package_resources.py"
        ).read_bytes(),
        "conductor/fixture.txt": b"conductor-installed-resource\n",
        "tooling/fixture.txt": b"tooling-installed-resource\n",
        "tooling/nested/fixture.txt": b"nested-installed-resource\n",
        "conductor_tooling-9.9.9.dist-info/METADATA": (
            b"Metadata-Version: 2.1\nName: conductor-tooling\nVersion: 9.9.9\n"
        ),
        "conductor_tooling-9.9.9.dist-info/WHEEL": (
            b"Wheel-Version: 1.0\nGenerator: package-resources-test\n"
            b"Root-Is-Purelib: true\nTag: py3-none-any\n"
        ),
    }
    record_name = "conductor_tooling-9.9.9.dist-info/RECORD"
    records = [
        f"{name},sha256={_record_digest(data)},{len(data)}"
        for name, data in sorted(files.items())
    ]
    files[record_name] = ("\n".join([*records, f"{record_name},,"]) + "\n").encode(
        "utf-8"
    )
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in files.items():
            archive.writestr(name, data)


@pytest.fixture(scope="session")
def installed_baseline(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """Create one actual no-dependency wheel installation for copied test environments."""

    baseline = tmp_path_factory.mktemp("package-resource-wheel")
    wheel = baseline / "conductor_tooling-9.9.9-py3-none-any.whl"
    _write_wheel(wheel)
    environment = baseline / "installed-baseline"
    venv.EnvBuilder(with_pip=True, clear=True).create(environment)
    interpreter = environment / "bin/python"
    subprocess.run(
        [str(interpreter), "-m", "pip", "install", "--no-deps", str(wheel)],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return environment


@pytest.fixture()
def installed_tooling(tmp_path: Path, installed_baseline: Path) -> tuple[Path, Path]:
    """Copy the actual wheel installation into a writable test-private environment."""

    environment = tmp_path / "private-environment"
    shutil.copytree(installed_baseline, environment, symlinks=True)
    interpreter = environment / "bin/python"
    site_packages = next((environment / "lib").glob("python*/site-packages"))
    return interpreter, site_packages


def _run_reader(
    interpreter: Path,
    *,
    package: str = "tooling",
    name: str = "fixture.txt",
    before_import: str = "",
    after_import: str = "",
) -> dict[str, object]:
    script = f"""
import hashlib
import json
{before_import}
from conductor.package_resources import read_package_resource
from conductor.project_context import ContextError
{after_import}
try:
    result = read_package_resource({package!r}, {name!r})
except ContextError as error:
    print(json.dumps({{"code": error.detail.code, "field": error.detail.field}}))
else:
    print(json.dumps({{
        "package": result.package,
        "name": result.name,
        "version": result.distribution_version,
        "data": result.data.decode("utf-8"),
        "sha256": result.sha256,
    }}))
"""
    completed = subprocess.run(
        [str(interpreter), "-I", "-B", "-c", script],
        check=True,
        capture_output=True,
        text=True,
        timeout=15,
    )
    return json.loads(completed.stdout)


def test_reads_verified_bytes_from_an_installed_unpacked_wheel(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    before = {
        path.relative_to(site_packages).as_posix(): hashlib.sha256(
            path.read_bytes()
        ).hexdigest()
        for path in site_packages.rglob("*")
        if path.is_file()
    }

    tooling = _run_reader(interpreter)
    conductor = _run_reader(interpreter, package="conductor")

    assert tooling == {
        "package": "tooling",
        "name": "fixture.txt",
        "version": "9.9.9",
        "data": "tooling-installed-resource\n",
        "sha256": hashlib.sha256(b"tooling-installed-resource\n").hexdigest(),
    }
    assert conductor["data"] == "conductor-installed-resource\n"
    after = {
        path.relative_to(site_packages).as_posix(): hashlib.sha256(
            path.read_bytes()
        ).hexdigest()
        for path in site_packages.rglob("*")
        if path.is_file()
    }
    assert after == before


@pytest.mark.parametrize(
    ("package", "name"),
    [
        ("other", "fixture.txt"),
        ("tooling", ""),
        ("tooling", "/fixture.txt"),
        ("tooling", "../fixture.txt"),
        ("tooling", "nested//fixture.txt"),
        ("tooling", "nested\\fixture.txt"),
        ("tooling", "fixture\x00.txt"),
    ],
)
def test_refuses_invalid_resource_selectors(
    installed_tooling: tuple[Path, Path], package: str, name: str
) -> None:
    interpreter, _ = installed_tooling
    assert (
        _run_reader(interpreter, package=package, name=name)["code"]
        == "RESOURCE_NAME_INVALID"
    )


def test_refuses_foreign_namespace_pollution(
    installed_tooling: tuple[Path, Path], tmp_path: Path
) -> None:
    interpreter, _ = installed_tooling
    foreign = tmp_path / "foreign"
    (foreign / "tooling").mkdir(parents=True)
    (foreign / "tooling/fixture.txt").write_bytes(b"foreign namespace bytes\n")

    result = _run_reader(
        interpreter,
        before_import=f"import sys\nsys.path.append({str(foreign)!r})",
    )

    assert result == {"code": "RESOURCE_LAYOUT_UNSUPPORTED", "field": "package"}


def test_refuses_actual_project_shadow_before_resource_traversal(
    installed_tooling: tuple[Path, Path], tmp_path: Path
) -> None:
    interpreter, _ = installed_tooling
    foreign = tmp_path / "foreign"
    (foreign / "tooling").mkdir(parents=True)
    (foreign / "tooling/__init__.py").write_bytes(b"")
    result = _run_reader(
        interpreter,
        before_import=f"import sys\nsys.path.insert(0, {str(foreign)!r})",
        after_import="""
import conductor.package_resources as package_resources
from conductor.project_context import ErrorDetail
package_resources.resources.files = lambda _package: (_ for _ in ()).throw(
    ContextError(ErrorDetail(
        code="RESOURCE_UNSAFE",
        field="package",
        message="untrusted resource backend was invoked",
    ))
)
""",
    )

    assert result == {"code": "RESOURCE_LAYOUT_UNSUPPORTED", "field": "package"}


def test_refuses_missing_record(installed_tooling: tuple[Path, Path]) -> None:
    interpreter, site_packages = installed_tooling
    record = site_packages / "conductor_tooling-9.9.9.dist-info/RECORD"

    record.unlink()
    assert _run_reader(interpreter)["code"] == "RESOURCE_LAYOUT_UNSUPPORTED"


def test_requires_exact_record_path_membership(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    record = site_packages / "conductor_tooling-9.9.9.dist-info/RECORD"
    root_asset = site_packages / "tooling/fixture.txt"
    nested_asset = site_packages / "tooling/nested/fixture.txt"
    nested_asset.write_bytes(root_asset.read_bytes())
    lines = [
        line
        for line in record.read_text(encoding="utf-8").splitlines()
        if not line.startswith("tooling/fixture.txt,")
    ]
    for index, line in enumerate(lines):
        if line.startswith("tooling/nested/fixture.txt,"):
            lines[index] = (
                f"tooling/nested/fixture.txt,sha256={_record_digest(root_asset.read_bytes())},"
                f"{root_asset.stat().st_size}"
            )
            break
    else:
        pytest.fail("fixture RECORD omitted tooling/nested/fixture.txt")
    matching_alternates = [
        row
        for row in csv.reader(lines)
        if row[0].startswith("tooling/") and Path(row[0]).name == root_asset.name
    ]
    assert [row[0] for row in matching_alternates] == ["tooling/nested/fixture.txt"]
    assert matching_alternates[0][1:] == [
        f"sha256={_record_digest(root_asset.read_bytes())}",
        str(root_asset.stat().st_size),
    ]
    record.write_text("\n".join(lines) + "\n", encoding="utf-8")

    assert _run_reader(interpreter) == {
        "code": "RESOURCE_NOT_FOUND",
        "field": "name",
    }


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        (1, "md5=not-a-wheel-digest"),
        (2, "not-a-size"),
    ],
)
def test_refuses_malformed_record_hash_and_size(
    installed_tooling: tuple[Path, Path], field: int, replacement: str
) -> None:
    interpreter, site_packages = installed_tooling
    record = site_packages / "conductor_tooling-9.9.9.dist-info/RECORD"
    lines = record.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines):
        if line.startswith("tooling/fixture.txt,"):
            parts = line.split(",")
            parts[field] = replacement
            lines[index] = ",".join(parts)
            break
    else:
        pytest.fail("fixture RECORD omitted tooling/fixture.txt")
    record.write_text("\n".join(lines) + "\n", encoding="utf-8")

    assert _run_reader(interpreter)["code"] == "RESOURCE_LAYOUT_UNSUPPORTED"


def test_refuses_record_declared_over_limit_before_read(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    record = site_packages / "conductor_tooling-9.9.9.dist-info/RECORD"
    lines = record.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines):
        if line.startswith("tooling/fixture.txt,"):
            parts = line.split(",")
            parts[2] = str(_MAX_RESOURCE_BYTES + 1)
            lines[index] = ",".join(parts)
            break
    else:
        pytest.fail("fixture RECORD omitted tooling/fixture.txt")
    record.write_text("\n".join(lines) + "\n", encoding="utf-8")

    assert _run_reader(interpreter)["code"] == "RESOURCE_TOO_LARGE"


def test_refuses_duplicate_installed_distribution(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    source = site_packages / "conductor_tooling-9.9.9.dist-info"
    duplicate = site_packages / "conductor_tooling-8.8.8.dist-info"
    shutil.copytree(source, duplicate)
    metadata = duplicate / "METADATA"
    metadata.write_text(
        metadata.read_text(encoding="utf-8").replace(
            "Version: 9.9.9", "Version: 8.8.8"
        ),
        encoding="utf-8",
    )

    assert _run_reader(interpreter)["code"] == "RESOURCE_LAYOUT_UNSUPPORTED"


def test_refuses_editable_and_malformed_direct_url_metadata(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    direct_url = site_packages / "conductor_tooling-9.9.9.dist-info/direct_url.json"
    direct_url.write_text('{"dir_info":{"editable":true}}', encoding="utf-8")
    assert _run_reader(interpreter)["code"] == "RESOURCE_LAYOUT_UNSUPPORTED"

    direct_url.write_text("not valid JSON", encoding="utf-8")
    assert _run_reader(interpreter)["code"] == "RESOURCE_LAYOUT_UNSUPPORTED"


def test_refuses_symlink_oversize_and_record_drift(
    installed_tooling: tuple[Path, Path], tmp_path: Path
) -> None:
    interpreter, site_packages = installed_tooling
    resource = site_packages / "tooling/fixture.txt"
    target = tmp_path / "target.txt"
    target.write_bytes(b"outside\n")
    resource.unlink()
    resource.symlink_to(target)
    assert _run_reader(interpreter)["code"] == "RESOURCE_UNSAFE"

    resource.unlink()
    resource.write_bytes(b"x" * (_MAX_RESOURCE_BYTES + 1))
    assert _run_reader(interpreter)["code"] == "RESOURCE_TOO_LARGE"

    resource.write_bytes(b"drifted-installed-resource\n")
    assert _run_reader(interpreter)["code"] == "RESOURCE_CHANGED"


def test_refuses_intermediate_symlink_and_requires_record_path_membership(
    installed_tooling: tuple[Path, Path], tmp_path: Path
) -> None:
    interpreter, site_packages = installed_tooling
    nested = site_packages / "tooling/nested"
    target = site_packages / "tooling/foreign-nested"
    target.mkdir()
    (target / "fixture.txt").write_bytes((nested / "fixture.txt").read_bytes())
    (nested / "fixture.txt").unlink()
    nested.rmdir()
    nested.symlink_to(target, target_is_directory=True)

    assert (
        _run_reader(interpreter, name="nested/fixture.txt")["code"] == "RESOURCE_UNSAFE"
    )


def test_refuses_asset_path_replaced_after_open(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    resource = site_packages / "tooling/fixture.txt"
    replacement = site_packages / "tooling/replacement.txt"
    replacement.write_bytes(b"tooling-installed-resource\n")
    result = _run_reader(
        interpreter,
        after_import=f"""
import os
import conductor.package_resources as package_resources
resource = {str(resource)!r}
replacement = {str(replacement)!r}
parent = os.path.dirname(resource)
parent_stat = os.stat(parent)
original_fstat = package_resources.os.fstat
swapped = [False]
def replace_after_open(descriptor):
    observed = original_fstat(descriptor)
    if os.path.stat.S_ISREG(observed.st_mode) and not swapped[0]:
        os.replace(replacement, resource)
        os.utime(
            parent,
            ns=(parent_stat.st_atime_ns, parent_stat.st_mtime_ns),
        )
        swapped[0] = True
    return observed
package_resources.os.fstat = replace_after_open
""",
    )

    assert result == {"code": "RESOURCE_CHANGED", "field": "name"}


def test_refuses_fifo_swapped_before_open_without_blocking(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    resource = site_packages / "tooling/fixture.txt"
    result = _run_reader(
        interpreter,
        after_import=f"""
import os
import conductor.package_resources as package_resources
resource = {str(resource)!r}
original_open = package_resources.os.open
def swap_to_fifo(path, flags, *, dir_fd=None):
    if dir_fd is not None:
        os.unlink(resource)
        os.mkfifo(resource)
        return original_open(path, flags, dir_fd=dir_fd)
    return original_open(path, flags)
package_resources.os.open = swap_to_fifo
""",
    )

    assert result == {"code": "RESOURCE_UNSAFE", "field": "name"}


def test_refuses_same_byte_symlink_swapped_between_validation_and_open(
    installed_tooling: tuple[Path, Path],
) -> None:
    interpreter, site_packages = installed_tooling
    resource = site_packages / "tooling/fixture.txt"
    target = site_packages / "same-byte-symlink-target.txt"
    target.write_bytes(resource.read_bytes())
    result = _run_reader(
        interpreter,
        after_import=f"""
import os
import conductor.package_resources as package_resources
resource = {str(resource)!r}
target = {str(target)!r}
original_open = package_resources.os.open
def swap_to_same_byte_symlink(path, flags, *, dir_fd=None):
    if dir_fd is not None:
        os.unlink(resource)
        os.symlink(target, resource)
        return original_open(path, flags, dir_fd=dir_fd)
    return original_open(path, flags)
package_resources.os.open = swap_to_same_byte_symlink
""",
    )

    assert result == {"code": "RESOURCE_UNSAFE", "field": "name"}


def test_anchored_walk_refuses_intermediate_directory_swapped_to_external_same_bytes(
    installed_tooling: tuple[Path, Path], tmp_path: Path
) -> None:
    interpreter, site_packages = installed_tooling
    nested = site_packages / "tooling/nested"
    external = tmp_path / "external"
    external.mkdir()
    (external / "fixture.txt").write_bytes((nested / "fixture.txt").read_bytes())
    result = _run_reader(
        interpreter,
        name="nested/fixture.txt",
        after_import=f"""
import os
import conductor.package_resources as package_resources
nested = {str(nested)!r}
external = {str(external)!r}
original_open = package_resources.os.open
def swap_intermediate(path, flags, *, dir_fd=None):
    if path == "nested" and dir_fd is not None:
        os.unlink(nested + "/fixture.txt")
        os.rmdir(nested)
        os.symlink(external, nested)
    if dir_fd is None:
        return original_open(path, flags)
    return original_open(path, flags, dir_fd=dir_fd)
package_resources.os.open = swap_intermediate
""",
    )

    assert result == {"code": "RESOURCE_UNSAFE", "field": "name"}
