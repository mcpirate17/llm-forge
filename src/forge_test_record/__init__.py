"""pytest plugin: record what each test run depended on, so the land can reuse passes.

Active only under a ``pyproject.toml`` that declares ``[tool.forge.test_record]``;
``FORGE_TEST_RECORD=0`` turns it off. The recording is native
(``conductor_native.TestRecorder``); this module only connects pytest's hooks,
``sys.monitoring`` and the audit hook to it. A recording failure is reported loudly
and never changes the session's exit status.
"""

from __future__ import annotations

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
_summary = ""


def _active(config: pytest.Config) -> bool:
    return _recorder is not None and config is _owner


def _host_places() -> tuple[list[str], list[str], str | None]:
    """Interpreter and installed-package prefixes, scratch directories and home."""
    paths = sysconfig.get_paths()
    prefixes = {sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix}
    prefixes.update(site.getsitepackages())
    prefixes.update(
        paths[name] for name in ("stdlib", "platstdlib", "purelib", "platlib")
    )
    if site.ENABLE_USER_SITE:
        prefixes.add(site.getusersitepackages())
    return sorted(prefixes), [tempfile.gettempdir()], os.environ.get("HOME")


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
        root, free[0], lines, environment, _host_places(), site.getsitepackages()
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
    if not _active(item.config):
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
    global _summary
    if config is _owner and _summary:
        terminalreporter.write_line(
            _summary, red=_summary.startswith("test record FAILED")
        )
        _summary = ""


def pytest_unconfigure(config: pytest.Config) -> None:
    global _owner, _recorder
    if config is _owner:
        _stop()
        if _recorder is not None:
            _recorder.close()
        _recorder, _owner = None, None
