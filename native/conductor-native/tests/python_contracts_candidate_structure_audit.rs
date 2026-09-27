#![cfg(feature = "python-compat-tests")]
//! Rust-owned positive and negative contracts for the structure audit.

#[path = "python_contracts/candidate_structure_support.rs"]
#[allow(dead_code)]
mod candidate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_support::{context, ChangeKind};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList};
use std::collections::HashSet;
use support::{module, text, Case};

type Triple = (String, String, i64);

fn run<'py>(
    py: Python<'py>,
    case: &Case,
    sources: &[(&str, &str)],
    changed: Option<&[&str]>,
) -> Bound<'py, PyAny> {
    for (relative, contents) in sources {
        case.write(&format!("snapshot/{relative}"), contents);
    }
    let selected = changed
        .map(|changed| changed.to_vec())
        .unwrap_or_else(|| sources.iter().map(|(path, _)| *path).collect());
    let ctx = context(
        py,
        &case.root().join("repo"),
        &case.root().join("snapshot"),
        &"b".repeat(40),
        &selected,
        &[],
        ChangeKind::PythonAdded,
    );
    module(py, "conductor.candidate_review.quality_checks")
        .getattr("check_structure_audit")
        .unwrap()
        .call1((ctx,))
        .unwrap()
}

fn findings<'py>(result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    result.getattr("findings").unwrap()
}

fn triples(result: &Bound<'_, PyAny>) -> Vec<Triple> {
    findings(result)
        .try_iter()
        .unwrap()
        .map(|item| {
            let item = item.unwrap();
            let rule = text(&item.getattr("rule_id").unwrap());
            let file = item
                .getattr("path")
                .unwrap()
                .extract::<Option<String>>()
                .unwrap()
                .unwrap_or_default();
            let line = item
                .getattr("line")
                .unwrap()
                .extract::<Option<i64>>()
                .unwrap()
                .unwrap_or(0);
            (rule, file, line)
        })
        .collect()
}

fn rules(triples: &[Triple]) -> HashSet<&str> {
    triples.iter().map(|(rule, _, _)| rule.as_str()).collect()
}

fn audit(case: &Case, sources: &[(&str, &str)], changed: Option<&[&str]>) -> Vec<Triple> {
    Python::attach(|py| triples(&run(py, case, sources, changed)))
}

const LEAK_TRUE_POSITIVE: &str = r#"import os


def read_locked(path):
    handle = os.open(path)
    payload = handle.read()
    handle.close()
    return payload
"#;

const LEAK_GUARDED_BY_FINALLY: &str = r#"import os


def read_locked(path):
    handle = os.open(path)
    try:
        return handle.read()
    finally:
        handle.close()
"#;

const LEAK_GUARDED_BY_WITH: &str = r#"import os


def read_locked(path):
    with os.open(path) as handle:
        return handle.read()
"#;

const LEAK_RELEASE_DELEGATED_TO_CALLER: &str = r#"import os


def acquire(path):
    handle = os.open(path)
    return handle
"#;

const CONNECTION_TRUE_POSITIVE: &str = r#"import sqlite3


def query(path):
    conn = sqlite3.connect(path)
    rows = conn.execute("select 1").fetchall()
    conn.close()
    return rows
"#;

const LOCK_ORDER_AB: &str = r#"import threading

alpha_lock = threading.Lock()
beta_lock = threading.Lock()


def forward():
    with alpha_lock:
        with beta_lock:
            return 1
"#;

const LOCK_ORDER_BA: &str = r#"from pkg.forward import alpha_lock, beta_lock


def backward():
    with beta_lock:
        with alpha_lock:
            return 2
"#;

const LOCK_ORDER_AB_AGAIN: &str = r#"from pkg.forward import alpha_lock, beta_lock


def also_forward():
    with alpha_lock:
        with beta_lock:
            return 3
"#;

const ABSTRACT_ONE_IMPL: &str = r#"from abc import ABC, abstractmethod


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
"#;

const ABSTRACT_TWO_IMPLS: &str = r#"from abc import ABC, abstractmethod


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


class SecondBackend(StorageBackend):
    def load(self, key):
        return 1

    def store(self, key, value):
        return None
"#;

const PROTOCOL_NO_NOMINAL_IMPL: &str = r#"from abc import abstractmethod
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
"#;

const ABSTRACT_SINGLE_METHOD: &str = r#"from abc import ABC, abstractmethod


class Hook(ABC):
    @abstractmethod
    def run(self):
        ...


class OnlyHook(Hook):
    def run(self):
        return None
"#;

const CONFIG_DEFAULT_LOCALHOST: &str = r#"import os

host = os.environ.get("SERVICE_HOST", "http://localhost:9000")
"#;

const CONFIG_DEFAULT_LOOPBACK: &str = r#"import os

host = os.getenv("SERVICE_HOST", "http://127.0.0.1:9000")
"#;

const CONFIG_DEFAULT_SHARED_CONSTANT: &str = r#"import os

from pkg.constants import SERVICE_HOST_DEFAULT

host = os.environ.get("SERVICE_HOST", SERVICE_HOST_DEFAULT)
"#;

const CONFIG_NO_DEFAULT: &str = r#"import os

host = os.environ.get("SERVICE_HOST")
"#;

const CACHE_GROWS_WITHOUT_EVICTION: &str = r#"_CACHE = {}


def lookup(key):
    if key not in _CACHE:
        _CACHE[key] = compute(key)
    return _CACHE[key]
"#;

const CACHE_WITH_EVICTION: &str = r#"_CACHE = {}


def lookup(key):
    if len(_CACHE) > 128:
        _CACHE.clear()
    _CACHE[key] = compute(key)
    return _CACHE[key]
"#;

const BOUNDED_DEQUE: &str = r#"import collections

_RECENT = collections.deque(maxlen=64)


def record(event):
    _RECENT.append(event)
"#;

const IMPORT_TIME_REGISTRY: &str = r#"_REGISTRY = {}
_REGISTRY["one"] = 1
_REGISTRY["two"] = 2


def lookup(key):
    return _REGISTRY[key]
"#;

#[test]
fn cleanup_flags_release_only_on_the_success_path() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/leak.py", LEAK_TRUE_POSITIVE)], None);
    assert!(result.contains(&(
        "cleanup-not-on-failure-path".to_owned(),
        "pkg/leak.py".to_owned(),
        5
    )));
}

#[test]
fn cleanup_ignores_release_in_finally() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/ok.py", LEAK_GUARDED_BY_FINALLY)], None);
    assert!(!rules(&result).contains("cleanup-not-on-failure-path"));
}

#[test]
fn cleanup_ignores_release_in_with_statement() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/ok.py", LEAK_GUARDED_BY_WITH)], None);
    assert!(!rules(&result).contains("cleanup-not-on-failure-path"));
}

#[test]
fn cleanup_ignores_ownership_transferred_to_caller() {
    let case = Case::new();
    let result = audit(
        &case,
        &[("pkg/ok.py", LEAK_RELEASE_DELEGATED_TO_CALLER)],
        None,
    );
    assert!(!rules(&result).contains("cleanup-not-on-failure-path"));
}

#[test]
fn connection_leak_gets_its_own_advisory_rule() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/db.py", CONNECTION_TRUE_POSITIVE)], None);
    assert!(result.contains(&(
        "connection-close-not-on-failure-path".to_owned(),
        "pkg/db.py".to_owned(),
        5
    )));
    assert!(!rules(&result).contains("cleanup-not-on-failure-path"));
}

#[test]
fn cleanup_finding_is_blocking() {
    let case = Case::new();
    Python::attach(|py| {
        let result = run(py, &case, &[("pkg/leak.py", LEAK_TRUE_POSITIVE)], None);
        let high = module(py, "conductor.candidate_review.model")
            .getattr("Severity")
            .unwrap()
            .getattr("HIGH")
            .unwrap();
        let leaks = findings(&result)
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .filter(|finding| {
                text(&finding.getattr("rule_id").unwrap()) == "cleanup-not-on-failure-path"
            })
            .collect::<Vec<_>>();
        assert_eq!(leaks.len(), 1);
        assert!(leaks[0].getattr("severity").unwrap().is(&high));
    });
}

#[test]
fn lock_order_flags_both_sides_of_an_inversion() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/forward.py", LOCK_ORDER_AB),
            ("pkg/backward.py", LOCK_ORDER_BA),
        ],
        None,
    );
    let flagged = result
        .iter()
        .filter(|(rule, _, _)| rule == "inconsistent-lock-order")
        .map(|(_, file, line)| (file.clone(), *line))
        .collect::<HashSet<_>>();
    assert_eq!(
        flagged,
        HashSet::from([
            ("pkg/forward.py".to_owned(), 9),
            ("pkg/backward.py".to_owned(), 6)
        ])
    );
}

#[test]
fn lock_order_ignores_a_consistent_global_order() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/forward.py", LOCK_ORDER_AB),
            ("pkg/again.py", LOCK_ORDER_AB_AGAIN),
        ],
        None,
    );
    assert!(!rules(&result).contains("inconsistent-lock-order"));
}

#[test]
fn lock_order_reports_only_the_changed_side() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/forward.py", LOCK_ORDER_AB),
            ("pkg/backward.py", LOCK_ORDER_BA),
        ],
        Some(&["pkg/backward.py"]),
    );
    let flagged = result
        .iter()
        .filter(|(rule, _, _)| rule == "inconsistent-lock-order")
        .map(|(_, file, line)| (file.as_str(), *line))
        .collect::<Vec<_>>();
    assert_eq!(flagged, [("pkg/backward.py", 6)]);
}

#[test]
fn abstraction_flags_two_method_interface_with_one_implementation() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/storage.py", ABSTRACT_ONE_IMPL)], None);
    assert!(result.contains(&(
        "single-implementation-abstraction".to_owned(),
        "pkg/storage.py".to_owned(),
        4
    )));
}

#[test]
fn abstraction_ignores_two_implementations() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/iface.py", ABSTRACT_TWO_IMPLS)], None);
    assert!(!rules(&result).contains("single-implementation-abstraction"));
}

#[test]
fn abstraction_ignores_structural_protocol() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/iface.py", PROTOCOL_NO_NOMINAL_IMPL)], None);
    assert!(!rules(&result).contains("single-implementation-abstraction"));
}

#[test]
fn abstraction_ignores_single_method_interface() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/iface.py", ABSTRACT_SINGLE_METHOD)], None);
    assert!(!rules(&result).contains("single-implementation-abstraction"));
}

#[test]
fn config_flags_two_defaults_for_one_key() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/a.py", CONFIG_DEFAULT_LOCALHOST),
            ("pkg/b.py", CONFIG_DEFAULT_LOOPBACK),
        ],
        None,
    );
    let flagged = result
        .iter()
        .filter(|(rule, _, _)| rule == "duplicate-config-default")
        .map(|(_, file, line)| (file.clone(), *line))
        .collect::<HashSet<_>>();
    assert_eq!(
        flagged,
        HashSet::from([("pkg/a.py".to_owned(), 3), ("pkg/b.py".to_owned(), 3)])
    );
}

#[test]
fn config_ignores_one_default_repeated_through_a_constant() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/a.py", CONFIG_DEFAULT_SHARED_CONSTANT),
            ("pkg/b.py", CONFIG_DEFAULT_SHARED_CONSTANT),
            ("pkg/c.py", CONFIG_NO_DEFAULT),
        ],
        None,
    );
    assert!(!rules(&result).contains("duplicate-config-default"));
}

#[test]
fn unbounded_state_flags_a_cache_that_only_grows() {
    let case = Case::new();
    let result = audit(
        &case,
        &[("pkg/cache.py", CACHE_GROWS_WITHOUT_EVICTION)],
        None,
    );
    assert!(result.contains(&(
        "unbounded-module-cache".to_owned(),
        "pkg/cache.py".to_owned(),
        6
    )));
}

#[test]
fn unbounded_state_ignores_cache_with_eviction() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/x.py", CACHE_WITH_EVICTION)], None);
    assert!(!rules(&result).contains("unbounded-module-cache"));
}

#[test]
fn unbounded_state_ignores_bounded_deque() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/x.py", BOUNDED_DEQUE)], None);
    assert!(!rules(&result).contains("unbounded-module-cache"));
}

#[test]
fn unbounded_state_ignores_import_time_registry() {
    let case = Case::new();
    let result = audit(&case, &[("pkg/x.py", IMPORT_TIME_REGISTRY)], None);
    assert!(!rules(&result).contains("unbounded-module-cache"));
}

#[test]
fn unbounded_state_stays_advisory() {
    let case = Case::new();
    Python::attach(|py| {
        let result = run(
            py,
            &case,
            &[("pkg/cache.py", CACHE_GROWS_WITHOUT_EVICTION)],
            None,
        );
        let medium = module(py, "conductor.candidate_review.model")
            .getattr("Severity")
            .unwrap()
            .getattr("MEDIUM")
            .unwrap();
        let caches = findings(&result)
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .filter(|finding| {
                text(&finding.getattr("rule_id").unwrap()) == "unbounded-module-cache"
            })
            .collect::<Vec<_>>();
        assert_eq!(caches.len(), 1);
        assert!(caches[0].getattr("severity").unwrap().is(&medium));
    });
}

#[test]
fn audit_is_silent_when_nothing_python_changed() {
    let case = Case::new();
    Python::attach(|py| {
        let result = run(py, &case, &[("pkg/leak.py", LEAK_TRUE_POSITIVE)], Some(&[]));
        assert!(findings(&result).eq(PyList::empty(py)).unwrap());
        let metrics = result.getattr("metrics").unwrap();
        let has_modules: bool = if metrics.is_none() {
            false
        } else {
            metrics
                .call_method1("__contains__", ("modules_indexed",))
                .unwrap()
                .extract()
                .unwrap()
        };
        assert!(!has_modules);
    });
}

#[test]
fn audit_skips_unparseable_modules_without_failing() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/broken.py", "def f(:\n"),
            ("pkg/leak.py", LEAK_TRUE_POSITIVE),
        ],
        Some(&["pkg/leak.py"]),
    );
    assert_eq!(
        rules(&result),
        HashSet::from(["cleanup-not-on-failure-path"])
    );
}

#[test]
fn audit_findings_are_sorted_by_path_then_line() {
    let case = Case::new();
    let result = audit(
        &case,
        &[
            ("pkg/a.py", CONFIG_DEFAULT_LOCALHOST),
            ("pkg/b.py", CONFIG_DEFAULT_LOOPBACK),
            ("pkg/z.py", LEAK_TRUE_POSITIVE),
        ],
        None,
    );
    let paths = result
        .iter()
        .map(|(_, file, _)| file.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"pkg/a.py"));
    assert_eq!(paths.last(), Some(&"pkg/z.py"));
    let mut sorted = result.clone();
    sorted.sort_by(|a, b| (&a.1, a.2, &a.0).cmp(&(&b.1, b.2, &b.0)));
    assert_eq!(result, sorted);
}
