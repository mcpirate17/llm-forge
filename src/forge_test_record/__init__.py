"""pytest plugin: record what each test run depended on, so the land can reuse passes.

Active only under a ``pyproject.toml`` that declares ``[tool.forge.test_record]``;
``FORGE_TEST_RECORD=0`` turns it off. The recording is native
(``conductor_native.TestRecorder``); this module only connects pytest's hooks,
``sys.monitoring`` and the audit hook to it. A recording failure is reported loudly
and never changes the session's exit status.

``FORGE_TEST_RECORD_REUSE=1`` skips each collected test whose newest run against
identical inputs passed, and records nothing for it. With
``FORGE_TEST_RECORD_REUSE_REPORT=<dir>`` the session also writes how many it skipped
to ``<dir>/<pid>.json``.
"""

from __future__ import annotations

import json
import os
import site
import sys
import sysconfig
import tempfile
import time

import pytest

# sys.monitoring reserves 0-2 and 5 for debuggers, coverage, profilers and optimizers.
_TOOL_IDS = (3, 4)
_FIXTURES = pytest.StashKey[list[str]]()

_recorder = None
_owner: pytest.Config | None = None  # nested in-process sessions are not recorded
_store = ""
_tool = -1
_running: str | None = None  # node id of the owner's test in progress
_reused: set[str] = set()  # node ids skipped on a recorded pass, never recorded
_summary = ""
_reuse_summary = ""


def _active(config: pytest.Config) -> bool:
    return _recorder is not None and config is _owner


def _host_places() -> tuple[list[str], list[str], str | None, list[str]]:
    """Interpreter and installed-package prefixes, scratch directories, home and the
    site directories whose distributions a test can depend on."""
    paths = sysconfig.get_paths()
    prefixes = {sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix}
    prefixes.update(site.getsitepackages())
    prefixes.update(
        paths[name] for name in ("stdlib", "platstdlib", "purelib", "platlib")
    )
    if site.ENABLE_USER_SITE:
        prefixes.add(site.getusersitepackages())
    home = os.environ.get("HOME")
    # A cache holds what was built from recorded inputs (torch extensions, compiled
    # kernels), and a build during the session would otherwise drop every test after it.
    scratch = {tempfile.gettempdir(), os.environ.get("XDG_CACHE_HOME")}
    if home:
        scratch.add(os.path.join(home, ".cache"))
    scratch_dirs = sorted(path for path in scratch if path)
    return sorted(prefixes), scratch_dirs, home, site.getsitepackages()


def _instrument_files() -> list[str]:
    """This plugin and the native recorder: they run in every test, so the package
    that ships them is no test's dependency."""
    return [__file__] + [
        module.__file__
        for name, module in list(sys.modules.items())
        if name.partition(".")[0] == "conductor_native"
        and getattr(module, "__file__", None)
    ]


def _start(config: pytest.Config) -> None:
    global _recorder, _owner, _store, _tool
    from conductor_native import TestRecorder, test_record_config_native

    resolved = test_record_config_native(str(config.rootpath))
    if resolved is None:
        return
    root, store, lines, environment = resolved
    monitoring = sys.monitoring
    free = [tool for tool in _TOOL_IDS if monitoring.get_tool(tool) is None]
    if not free:
        raise pytest.UsageError(
            f"test record: sys.monitoring tools {_TOOL_IDS} are both in use; "
            "set FORGE_TEST_RECORD=0 to run without recording"
        )
    recorder = TestRecorder(
        root,
        free[0],
        lines,
        environment,
        _host_places(),
        _instrument_files(),
    )
    monitoring.use_tool_id(free[0], "forge test record")
    monitoring.register_callback(free[0], monitoring.events.PY_START, recorder.on_start)
    monitoring.register_callback(free[0], monitoring.events.LINE, recorder.on_line)
    monitoring.set_events(free[0], monitoring.events.PY_START)
    sys.addaudithook(recorder.on_audit)
    _recorder, _owner, _store, _tool = recorder, config, store, free[0]


def _stop() -> None:
    global _tool
    if _tool < 0:
        return
    monitoring = sys.monitoring
    monitoring.set_events(_tool, 0)
    monitoring.register_callback(_tool, monitoring.events.PY_START, None)
    monitoring.register_callback(_tool, monitoring.events.LINE, None)
    monitoring.free_tool_id(_tool)
    _tool = -1


@pytest.hookimpl(tryfirst=True)
def pytest_load_initial_conftests(early_config: pytest.Config) -> None:
    if _recorder is None and os.environ.get("FORGE_TEST_RECORD") != "0":
        _start(early_config)


def _reuse_report(collected: int, reused: int, seconds: float) -> None:
    directory = os.environ.get("FORGE_TEST_RECORD_REUSE_REPORT")
    if not directory:
        return
    os.makedirs(directory, exist_ok=True)
    report = {"collected": collected, "reused": reused, "recorded_seconds": seconds}
    path = os.path.join(directory, f"{os.getpid()}.json")
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(report, handle)


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]):
    global _reuse_summary
    if not _active(config) or os.environ.get("FORGE_TEST_RECORD_REUSE") != "1":
        return
    tests = [(item.nodeid, str(item.path)) for item in items]
    try:
        found = _recorder.reusable(_store, tests)
    except RuntimeError as error:
        _reuse_summary = f"test record reuse FAILED, every test runs: {error}"
        print(_reuse_summary, file=sys.stderr)
        return
    seconds = 0.0
    for item, hit in zip(items, found, strict=True):
        if hit is None:
            continue
        session, recorded = hit
        reason = (
            f"forge test record: passed in session {session} against identical inputs"
        )
        item.add_marker(pytest.mark.skip(reason=reason))
        _reused.add(item.nodeid)
        seconds += recorded
    _reuse_summary = (
        f"test record reuse: {len(_reused)} of {len(items)} tests passed before "
        f"against identical inputs and were skipped ({seconds:.1f}s recorded)"
    )
    _reuse_report(len(items), len(_reused), seconds)


@pytest.hookimpl(wrapper=True)
def pytest_make_collect_report(collector: pytest.Collector):
    if not (_active(collector.config) and isinstance(collector, pytest.Module)):
        return (yield)
    _recorder.push("collect", collector.nodeid, True)
    try:
        return (yield)
    finally:
        _recorder.pop("collect", collector.nodeid)


def _fixture_key(fixturedef: pytest.FixtureDef) -> str:
    return f"{fixturedef.baseid}::{fixturedef.argname}"


@pytest.hookimpl(wrapper=True)
def pytest_fixture_setup(fixturedef: pytest.FixtureDef, request: pytest.FixtureRequest):
    if not _active(request.config):
        return (yield)
    key = _fixture_key(fixturedef)
    # A function-scoped fixture is set up inside its test, after the test's restart.
    _recorder.push("fixture", key, fixturedef.scope != "function")
    try:
        return (yield)
    finally:
        _recorder.pop("fixture", key)


def _fixture_keys(item: pytest.Item) -> list[str]:
    """Every fixture the test requested or set up, read before pytest drops them."""
    if not isinstance(item, pytest.Function):
        return []
    try:
        info = item._fixtureinfo
        defs = list(item._request._fixture_defs.values())
        defs += [
            info.name2fixturedefs[name][-1]
            for name in info.names_closure
            if name in info.name2fixturedefs
        ]
    except AttributeError as error:
        _recorder.fail(
            f"pytest {pytest.__version__} fixture internals changed: {error}"
        )
        return []
    return sorted({_fixture_key(fixturedef) for fixturedef in defs})


@pytest.hookimpl(wrapper=True)
def pytest_runtest_teardown(item: pytest.Item):
    if _active(item.config):
        item.stash[_FIXTURES] = _fixture_keys(item)
    return (yield)


@pytest.hookimpl(wrapper=True)
def pytest_runtest_protocol(item: pytest.Item):
    if not _active(item.config) or item.nodeid in _reused:
        return (yield)
    global _running
    _recorder.push("test", item.nodeid, True)
    _running = item.nodeid
    started = time.perf_counter()
    try:
        return (yield)
    finally:
        _running = None
        module = item.getparent(pytest.Module)
        _recorder.end_test(
            item.nodeid,
            str(item.path),
            None if module is None else module.nodeid,
            item.stash.get(_FIXTURES, []),
            time.perf_counter() - started,
        )


def pytest_runtest_logreport(report: pytest.TestReport) -> None:
    # A report carries no config: a nested session's reports name other node ids.
    if _recorder is not None and report.nodeid == _running:
        _recorder.report(
            report.nodeid, report.when, report.outcome, hasattr(report, "wasxfail")
        )


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session: pytest.Session) -> None:
    global _recorder, _summary
    if not _active(session.config):
        return
    _stop()
    recorder, _recorder = _recorder, None
    inipath = session.config.inipath
    try:
        recorded = recorder.finish(_store, None if inipath is None else str(inipath))
        _summary = f"test record: {recorded}"
    except RuntimeError as error:
        _summary = f"test record FAILED, nothing recorded: {error}"
        print(_summary, file=sys.stderr)


def pytest_terminal_summary(terminalreporter, config: pytest.Config) -> None:
    global _summary, _reuse_summary
    if config is not _owner:
        return
    for line in (_reuse_summary, _summary):
        if line:
            failed = line.startswith(("test record FAILED", "test record reuse FAILED"))
            terminalreporter.write_line(line, red=failed)
    _summary = _reuse_summary = ""


def pytest_unconfigure(config: pytest.Config) -> None:
    global _owner, _recorder
    if config is _owner:
        _stop()
        if _recorder is not None:
            _recorder.close()
        _recorder, _owner = None, None
        _reused.clear()
