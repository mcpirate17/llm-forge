"""Deterministic true-positive / false-positive fixtures for the structure-audit check.

Every rule gets a matched pair: a fixture the rule must flag, and a fixture that is the
same shape done correctly, which the rule must not flag. The false-positive fixtures are
the load-bearing half -- they encode the exact idioms the rule was measured against on
the tracked tree (Protocol bases, import-time registry population, bounded containers,
with-statement lifetimes, releases already in a finally) and they are what stops a
future widening of a pattern from turning the gate into noise.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from conductor.candidate_review.checks import ReviewContext
from conductor.candidate_review.model import Candidate, Change, CheckResult, Severity
from conductor.candidate_review.policy import load_policy
from conductor.candidate_review.quality_checks import check_structure_audit

POLICY = load_policy(Path("conductor/candidate_policy.toml"))


def _change(path: str) -> Change:
    return Change(
        status="A",
        path=path,
        old_path=None,
        old_mode="000000",
        new_mode="100644",
        old_oid="0" * 40,
        new_oid="1" * 40,
        classes=("python", "source"),
    )


def _run(
    tmp_path: Path, sources: dict[str, str], *, changed: tuple[str, ...] | None = None
) -> CheckResult:
    """Run the check over a synthetic snapshot and return the whole result."""

    snapshot = tmp_path / "snapshot"
    for rel, text in sources.items():
        path = snapshot / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    selected = changed if changed is not None else tuple(sources)
    context = ReviewContext(
        repo=tmp_path / "repo",
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=tuple(_change(rel) for rel in selected),
        ),
        entries=(),
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    return check_structure_audit(context)


def _audit(
    tmp_path: Path, sources: dict[str, str], *, changed: tuple[str, ...] | None = None
) -> list[tuple[str, str, int]]:
    """Run the check over a synthetic snapshot; return (rule_id, path, line) triples."""

    return [
        (finding.rule_id, finding.path or "", finding.line or 0)
        for finding in _run(tmp_path, sources, changed=changed).findings
    ]


def _rules(triples: list[tuple[str, str, int]]) -> set[str]:
    return {rule for rule, _, _ in triples}


# --- 3. resource leaks on the failure path ----------------------------------------


LEAK_TRUE_POSITIVE = """\
import os


def read_locked(path):
    handle = os.open(path)
    payload = handle.read()
    handle.close()
    return payload
"""

LEAK_GUARDED_BY_FINALLY = """\
import os


def read_locked(path):
    handle = os.open(path)
    try:
        return handle.read()
    finally:
        handle.close()
"""

LEAK_GUARDED_BY_WITH = """\
import os


def read_locked(path):
    with os.open(path) as handle:
        return handle.read()
"""

LEAK_RELEASE_DELEGATED_TO_CALLER = """\
import os


def acquire(path):
    handle = os.open(path)
    return handle
"""


def test_cleanup_flags_release_only_on_the_success_path(tmp_path: Path) -> None:
    triples = _audit(tmp_path, {"pkg/leak.py": LEAK_TRUE_POSITIVE})
    assert ("cleanup-not-on-failure-path", "pkg/leak.py", 5) in triples


@pytest.mark.parametrize(
    "source",
    [LEAK_GUARDED_BY_FINALLY, LEAK_GUARDED_BY_WITH, LEAK_RELEASE_DELEGATED_TO_CALLER],
    ids=["finally", "with-statement", "ownership-transferred"],
)
def test_cleanup_ignores_structurally_guaranteed_release(
    tmp_path: Path, source: str
) -> None:
    assert "cleanup-not-on-failure-path" not in _rules(
        _audit(tmp_path, {"pkg/ok.py": source})
    )


CONNECTION_TRUE_POSITIVE = """\
import sqlite3


def query(path):
    conn = sqlite3.connect(path)
    rows = conn.execute("select 1").fetchall()
    conn.close()
    return rows
"""


def test_connection_leak_gets_its_own_advisory_rule(tmp_path: Path) -> None:
    """Same defect shape, 149 pre-existing sites: real, but it must not block a merge."""

    triples = _audit(tmp_path, {"pkg/db.py": CONNECTION_TRUE_POSITIVE})
    assert ("connection-close-not-on-failure-path", "pkg/db.py", 5) in triples
    assert "cleanup-not-on-failure-path" not in _rules(triples)


def test_cleanup_finding_is_blocking(tmp_path: Path) -> None:
    """Severity is the whole point of this rule; assert it, do not assume it."""

    snapshot = tmp_path / "snapshot"
    (snapshot / "pkg").mkdir(parents=True)
    (snapshot / "pkg" / "leak.py").write_text(LEAK_TRUE_POSITIVE, encoding="utf-8")
    context = ReviewContext(
        repo=tmp_path / "repo",
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(_change("pkg/leak.py"),),
        ),
        entries=(),
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    findings = check_structure_audit(context).findings
    leaks = [f for f in findings if f.rule_id == "cleanup-not-on-failure-path"]
    assert [f.severity for f in leaks] == [Severity.HIGH]


# --- 4. inconsistent lock ordering -------------------------------------------------


LOCK_ORDER_AB = """\
import threading

alpha_lock = threading.Lock()
beta_lock = threading.Lock()


def forward():
    with alpha_lock:
        with beta_lock:
            return 1
"""

LOCK_ORDER_BA = """\
from pkg.forward import alpha_lock, beta_lock


def backward():
    with beta_lock:
        with alpha_lock:
            return 2
"""

LOCK_ORDER_AB_AGAIN = """\
from pkg.forward import alpha_lock, beta_lock


def also_forward():
    with alpha_lock:
        with beta_lock:
            return 3
"""


def test_lock_order_flags_both_sides_of_an_inversion(tmp_path: Path) -> None:
    triples = _audit(
        tmp_path, {"pkg/forward.py": LOCK_ORDER_AB, "pkg/backward.py": LOCK_ORDER_BA}
    )
    flagged = {
        (path, line)
        for rule, path, line in triples
        if rule == "inconsistent-lock-order"
    }
    # Anchored at the inner `with` -- the statement that takes the second lock, which
    # is the line that has to move to fix the inversion.
    assert flagged == {("pkg/forward.py", 9), ("pkg/backward.py", 6)}


def test_lock_order_ignores_a_consistent_global_order(tmp_path: Path) -> None:
    triples = _audit(
        tmp_path,
        {"pkg/forward.py": LOCK_ORDER_AB, "pkg/again.py": LOCK_ORDER_AB_AGAIN},
    )
    assert "inconsistent-lock-order" not in _rules(triples)


def test_lock_order_reports_only_the_changed_side(tmp_path: Path) -> None:
    """The inversion exists tree-wide; only the candidate's own file is its regression."""

    triples = _audit(
        tmp_path,
        {"pkg/forward.py": LOCK_ORDER_AB, "pkg/backward.py": LOCK_ORDER_BA},
        changed=("pkg/backward.py",),
    )
    flagged = [
        (path, line)
        for rule, path, line in triples
        if rule == "inconsistent-lock-order"
    ]
    assert flagged == [("pkg/backward.py", 6)]


# --- 1. single-implementation abstraction ------------------------------------------


ABSTRACT_ONE_IMPL = """\
from abc import ABC, abstractmethod


class StorageBackend(ABC):
    @abstractmethod
    def load(self, key):
        ...

    @abstractmethod
    def store(self, key, value):
        ...


class OnlyBackend(StorageBackend):
    def load(self, key):
        return None

    def store(self, key, value):
        return None
"""

ABSTRACT_TWO_IMPLS = (
    ABSTRACT_ONE_IMPL
    + """\


class SecondBackend(StorageBackend):
    def load(self, key):
        return 1

    def store(self, key, value):
        return None
"""
)

# Structural typing: the protocol is abstract by every test the rule applies, has two
# methods, and has zero nominal subclasses -- so only the `Protocol` carve-out keeps it
# from being reported. RedisStore satisfies it structurally without inheriting.
PROTOCOL_NO_NOMINAL_IMPL = """\
from abc import abstractmethod
from typing import Protocol


class StorageBackend(Protocol):
    @abstractmethod
    def load(self, key): ...

    @abstractmethod
    def store(self, key, value): ...


class RedisStore:
    def load(self, key):
        return None

    def store(self, key, value):
        return None
"""

ABSTRACT_SINGLE_METHOD = """\
from abc import ABC, abstractmethod


class Hook(ABC):
    @abstractmethod
    def run(self):
        ...


class OnlyHook(Hook):
    def run(self):
        return None
"""


def test_abstraction_flags_a_two_method_interface_with_one_implementation(
    tmp_path: Path,
) -> None:
    triples = _audit(tmp_path, {"pkg/storage.py": ABSTRACT_ONE_IMPL})
    assert ("single-implementation-abstraction", "pkg/storage.py", 4) in triples


@pytest.mark.parametrize(
    ("name", "source"),
    [
        ("two-implementations", ABSTRACT_TWO_IMPLS),
        ("structural-protocol", PROTOCOL_NO_NOMINAL_IMPL),
        ("single-method-interface", ABSTRACT_SINGLE_METHOD),
    ],
    ids=lambda value: value if isinstance(value, str) and " " not in value else "",
)
def test_abstraction_ignores_justified_interfaces(
    tmp_path: Path, name: str, source: str
) -> None:
    assert "single-implementation-abstraction" not in _rules(
        _audit(tmp_path, {"pkg/iface.py": source})
    )


# --- 6. duplicate configuration defaults -------------------------------------------


CONFIG_DEFAULT_LOCALHOST = """\
import os

host = os.environ.get("SERVICE_HOST", "http://localhost:9000")
"""

CONFIG_DEFAULT_LOOPBACK = """\
import os

host = os.getenv("SERVICE_HOST", "http://127.0.0.1:9000")
"""

CONFIG_DEFAULT_SHARED_CONSTANT = """\
import os

from pkg.constants import SERVICE_HOST_DEFAULT

host = os.environ.get("SERVICE_HOST", SERVICE_HOST_DEFAULT)
"""

CONFIG_NO_DEFAULT = """\
import os

host = os.environ.get("SERVICE_HOST")
"""


def test_config_flags_two_defaults_for_one_key(tmp_path: Path) -> None:
    triples = _audit(
        tmp_path,
        {"pkg/a.py": CONFIG_DEFAULT_LOCALHOST, "pkg/b.py": CONFIG_DEFAULT_LOOPBACK},
    )
    flagged = {
        (path, line)
        for rule, path, line in triples
        if rule == "duplicate-config-default"
    }
    assert flagged == {("pkg/a.py", 3), ("pkg/b.py", 3)}


def test_config_ignores_one_default_repeated_through_a_constant(tmp_path: Path) -> None:
    triples = _audit(
        tmp_path,
        {
            "pkg/a.py": CONFIG_DEFAULT_SHARED_CONSTANT,
            "pkg/b.py": CONFIG_DEFAULT_SHARED_CONSTANT,
            "pkg/c.py": CONFIG_NO_DEFAULT,
        },
    )
    assert "duplicate-config-default" not in _rules(triples)


# --- 2. unbounded mutable module state ---------------------------------------------


CACHE_GROWS_WITHOUT_EVICTION = """\
_CACHE = {}


def lookup(key):
    if key not in _CACHE:
        _CACHE[key] = compute(key)
    return _CACHE[key]
"""

CACHE_WITH_EVICTION = """\
_CACHE = {}


def lookup(key):
    if len(_CACHE) > 128:
        _CACHE.clear()
    _CACHE[key] = compute(key)
    return _CACHE[key]
"""

BOUNDED_DEQUE = """\
import collections

_RECENT = collections.deque(maxlen=64)


def record(event):
    _RECENT.append(event)
"""

IMPORT_TIME_REGISTRY = """\
_REGISTRY = {}
_REGISTRY["one"] = 1
_REGISTRY["two"] = 2


def lookup(key):
    return _REGISTRY[key]
"""


def test_unbounded_state_flags_a_cache_that_only_grows(tmp_path: Path) -> None:
    triples = _audit(tmp_path, {"pkg/cache.py": CACHE_GROWS_WITHOUT_EVICTION})
    assert ("unbounded-module-cache", "pkg/cache.py", 6) in triples


@pytest.mark.parametrize(
    "source",
    [CACHE_WITH_EVICTION, BOUNDED_DEQUE, IMPORT_TIME_REGISTRY],
    ids=["has-eviction", "bounded-maxlen", "populated-at-import"],
)
def test_unbounded_state_ignores_bounded_containers(
    tmp_path: Path, source: str
) -> None:
    assert "unbounded-module-cache" not in _rules(
        _audit(tmp_path, {"pkg/x.py": source})
    )


def test_unbounded_state_stays_advisory(tmp_path: Path) -> None:
    """MEDIUM keeps this out of the block_at="high" set while its debt is paid down."""

    snapshot = tmp_path / "snapshot"
    (snapshot / "pkg").mkdir(parents=True)
    (snapshot / "pkg" / "cache.py").write_text(
        CACHE_GROWS_WITHOUT_EVICTION, encoding="utf-8"
    )
    context = ReviewContext(
        repo=tmp_path / "repo",
        snapshot=snapshot,
        candidate=Candidate(
            kind="index",
            tree_oid="a" * 40,
            base_tree_oid="b" * 40,
            base_commit_oid="c" * 40,
            commit_oid=None,
            target_ref="HEAD",
            changes=(_change("pkg/cache.py"),),
        ),
        entries=(),
        policy=POLICY,
        surface="manual",
        profile="full",
        owner=None,
        runtime_dir=tmp_path / "runtime",
    )
    caches = [
        finding
        for finding in check_structure_audit(context).findings
        if finding.rule_id == "unbounded-module-cache"
    ]
    assert [finding.severity for finding in caches] == [Severity.MEDIUM]


# --- shared behaviour ---------------------------------------------------------------


def test_audit_is_silent_when_nothing_python_changed(tmp_path: Path) -> None:
    result = _run(tmp_path, {"pkg/leak.py": LEAK_TRUE_POSITIVE}, changed=())
    assert result.findings == []
    # Silence is not enough: the check must also skip the walk entirely. Without this
    # the short circuit can be deleted and nothing fails, because every rule is already
    # gated on the changed set.
    assert "modules_indexed" not in (result.metrics or {})


def test_audit_skips_unparseable_modules_without_failing(tmp_path: Path) -> None:
    """python-ast already reports python-parse; this check must not double-report."""

    triples = _audit(
        tmp_path,
        {"pkg/broken.py": "def f(:\n", "pkg/leak.py": LEAK_TRUE_POSITIVE},
        changed=("pkg/leak.py",),
    )
    assert _rules(triples) == {"cleanup-not-on-failure-path"}


def test_audit_findings_are_sorted_by_path_then_line(tmp_path: Path) -> None:
    triples = _audit(
        tmp_path,
        {
            "pkg/a.py": CONFIG_DEFAULT_LOCALHOST,
            "pkg/b.py": CONFIG_DEFAULT_LOOPBACK,
            "pkg/z.py": LEAK_TRUE_POSITIVE,
        },
    )
    # Discovery order is cleanup-first (emitted inside the module walk, at pkg/z.py)
    # and config-last (emitted after it, at pkg/a.py and pkg/b.py), so comparing against
    # sorted() alone would hold even with the sort removed -- the last-emitted finding
    # has to come out first. Assert that, not the exact finding set: this test is about
    # ordering, and pinning the set here would steal the config rule's own coverage.
    paths = [path for _, path, _ in triples]
    assert "pkg/a.py" in paths
    assert paths[-1] == "pkg/z.py"
    assert triples == sorted(triples, key=lambda item: (item[1], item[2], item[0]))
