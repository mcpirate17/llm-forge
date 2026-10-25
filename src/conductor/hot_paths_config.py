"""The folders a host declares hot, from ``[tool.forge.hot_paths] paths``.

A host opts in by listing the directories whose Python is compute: a loop over a
tensor there is throughput lost. A host without the table has no hot folders.
"""

import tomllib
from pathlib import Path, PurePosixPath


def hot_paths(root: Path) -> tuple[str, ...]:
    """The hot folders under `root`, empty when its pyproject does not declare any."""
    pyproject = root / "pyproject.toml"
    if not pyproject.is_file():
        return ()
    tool = tomllib.loads(pyproject.read_text(encoding="utf-8")).get("tool", {})
    table = tool.get("forge", {}).get("hot_paths")
    if table is None:
        return ()
    paths = table.get("paths")
    if (
        set(table) - {"paths"}
        or not isinstance(paths, list)
        or not paths
        or not all(
            isinstance(entry, str)
            and entry
            and not PurePosixPath(entry).is_absolute()
            and ".." not in PurePosixPath(entry).parts
            for entry in paths
        )
    ):
        raise ValueError(
            f"{pyproject}: [tool.forge.hot_paths] takes one key, paths, "
            "a non-empty list of directories relative to the host root"
        )
    return tuple(paths)
