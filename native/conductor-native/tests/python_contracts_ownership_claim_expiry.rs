#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for claim estimates, idle lapse, and legacy stores.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, text, Case};

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "--quiet"]);
    repo
}

fn ownership<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.ownership")
}

fn number(own: &Bound<'_, PyModule>, name: &str) -> f64 {
    own.getattr(name).unwrap().extract().unwrap()
}

fn cap(own: &Bound<'_, PyModule>) -> f64 {
    number(own, "MAX_ACTIVE_CLAIM_HOURS") * 60.0
}

fn delta<'py>(py: Python<'py>, unit: &str, value: f64) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(unit, value).unwrap();
    PyModule::import(py, "datetime")
        .unwrap()
        .getattr("timedelta")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn now<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let datetime = PyModule::import(py, "datetime").unwrap();
    datetime
        .getattr("datetime")
        .unwrap()
        .call_method1(
            "now",
            (datetime
                .getattr("timezone")
                .unwrap()
                .getattr("utc")
                .unwrap(),),
        )
        .unwrap()
}

fn add<'py>(instant: &Bound<'py, PyAny>, amount: Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    instant.call_method1("__add__", (amount,)).unwrap()
}

fn sub<'py>(left: &Bound<'py, PyAny>, right: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    left.call_method1("__sub__", (right,)).unwrap()
}

fn same(left: &Bound<'_, PyAny>, right: &Bound<'_, PyAny>) {
    assert!(left.eq(right).unwrap(), "{} != {}", text(left), text(right));
}

fn make<'py>(
    py: Python<'py>,
    own: &Bound<'py, PyModule>,
    repo: &Path,
    expected: f64,
    max: f64,
    owner: &str,
    target: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("owner", owner).unwrap();
    kwargs.set_item("paths", [target]).unwrap();
    kwargs.set_item("justification", "j").unwrap();
    kwargs.set_item("expected_minutes", expected).unwrap();
    kwargs.set_item("max_minutes", max).unwrap();
    own.getattr("create_claim")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))
}

fn ordinary<'py>(py: Python<'py>, own: &Bound<'py, PyModule>, repo: &Path) -> Bound<'py, PyAny> {
    make(py, own, repo, 15.0, 60.0, "alpha", "pkg/mod.py").unwrap()
}

fn only<'py>(py: Python<'py>, own: &Bound<'py, PyModule>, repo: &Path) -> Bound<'py, PyAny> {
    let claims = own
        .getattr("load_claims")
        .unwrap()
        .call1((path(py, repo),))
        .unwrap()
        .get_item(0)
        .unwrap();
    assert_eq!(claims.len().unwrap(), 1);
    claims.get_item(0).unwrap()
}

fn active(claim: &Bound<'_, PyAny>, instant: &Bound<'_, PyAny>) -> bool {
    claim
        .call_method1("active", (instant,))
        .unwrap()
        .extract()
        .unwrap()
}

fn overrun(claim: &Bound<'_, PyAny>, instant: &Bound<'_, PyAny>) -> bool {
    claim
        .call_method1("overrun", (instant,))
        .unwrap()
        .extract()
        .unwrap()
}

fn touch(
    py: Python<'_>,
    own: &Bound<'_, PyModule>,
    repo: &Path,
    claim_id: &str,
    instant: &Bound<'_, PyAny>,
) -> bool {
    let kwargs = PyDict::new(py);
    kwargs.set_item("now", instant).unwrap();
    own.getattr("touch_claim")
        .unwrap()
        .call((path(py, repo), claim_id), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

fn store_path(py: Python<'_>, own: &Bound<'_, PyModule>, repo: &Path) -> PathBuf {
    PathBuf::from(text(
        &own.getattr("claim_store_path")
            .unwrap()
            .call1((path(py, repo),))
            .unwrap(),
    ))
}

fn activity_path(py: Python<'_>, own: &Bound<'_, PyModule>, repo: &Path) -> PathBuf {
    PathBuf::from(text(
        &own.getattr("claim_activity_path")
            .unwrap()
            .call1((path(py, repo),))
            .unwrap(),
    ))
}

fn legacy_claim(py: Python<'_>, own: &Bound<'_, PyModule>, repo: &Path, hours: f64) -> String {
    let created = now(py);
    ordinary(py, own, repo);
    let store = store_path(py, own, repo);
    let json = PyModule::import(py, "json").unwrap();
    let payload = json
        .getattr("loads")
        .unwrap()
        .call1((fs::read_to_string(&store).unwrap(),))
        .unwrap();
    let entry = payload
        .get_item("claims")
        .unwrap()
        .get_item(0)
        .unwrap()
        .cast_into::<PyDict>()
        .unwrap();
    entry
        .call_method1("pop", ("expected_at", py.None()))
        .unwrap();
    let created_text = text(&created.call_method0("isoformat").unwrap());
    let expires_text = text(
        &add(&created, delta(py, "hours", hours))
            .call_method0("isoformat")
            .unwrap(),
    );
    entry.set_item("created_at", &created_text).unwrap();
    entry.set_item("expires_at", &expires_text).unwrap();
    let fields = PyDict::new(py);
    for name in ["owner", "justification", "created_at", "expires_at"] {
        fields
            .set_item(name, entry.get_item(name).unwrap().unwrap())
            .unwrap();
    }
    let paths = PyModule::import(py, "builtins")
        .unwrap()
        .getattr("tuple")
        .unwrap()
        .call1((entry.get_item("paths").unwrap().unwrap(),))
        .unwrap();
    fields.set_item("paths", paths).unwrap();
    let hash = text(
        &module(py, "conductor.candidate_review.model")
            .getattr("sha256_json")
            .unwrap()
            .call1((fields,))
            .unwrap(),
    );
    let claim_id = format!("claim-{}", &hash[..20]);
    entry.set_item("claim_id", &claim_id).unwrap();
    fs::write(
        store,
        text(&json.getattr("dumps").unwrap().call1((payload,)).unwrap()),
    )
    .unwrap();
    claim_id
}

#[test]
fn create_refuses_a_max_above_the_ceiling() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let error = make(py, &own, &repo, 1.0, cap(&own) + 1.0, "alpha", "pkg/mod.py").unwrap_err();
        assert_error(
            py,
            error,
            &own.getattr("OwnershipError").unwrap(),
            "claim max time must be",
        );
    });
}

#[test]
fn create_accepts_a_max_at_the_ceiling() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, 1.0, cap(&own), "alpha", "pkg/mod.py").unwrap();
        same(
            &sub(
                &claim.getattr("expiry").unwrap(),
                &claim.getattr("creation").unwrap(),
            ),
            &delta(py, "minutes", cap(&own)),
        );
    });
}

#[test]
fn create_refuses_an_expected_beyond_its_own_max() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let error = make(py, &own, &repo, 61.0, 60.0, "alpha", "pkg/mod.py").unwrap_err();
        assert_error(
            py,
            error,
            &own.getattr("OwnershipError").unwrap(),
            "expected time must be",
        );
    });
}

#[test]
fn create_stores_both_durations() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = ordinary(py, &own, &repo);
        let creation = claim.getattr("creation").unwrap();
        let expected = claim.getattr("expected").unwrap();
        let expiry = claim.getattr("expiry").unwrap();
        same(&sub(&expected, &creation), &delta(py, "minutes", 15.0));
        same(&sub(&expiry, &creation), &delta(py, "minutes", 60.0));
        assert!(expected.lt(&expiry).unwrap());
    });
}

#[test]
fn an_estimate_that_produced_no_writes_lapses_at_the_estimate() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = ordinary(py, &own, &repo);
        let creation = claim.getattr("creation").unwrap();
        assert!(active(&claim, &add(&creation, delta(py, "minutes", 14.0))));
        assert!(!active(&claim, &add(&creation, delta(py, "minutes", 16.0))));
        assert!(claim
            .getattr("expiry")
            .unwrap()
            .gt(add(&creation, delta(py, "minutes", 59.0)))
            .unwrap());
    });
}

#[test]
fn an_on_time_claim_keeps_the_long_idle_window() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, cap(&own), cap(&own), "alpha", "pkg/mod.py").unwrap();
        let creation = claim.getattr("creation").unwrap();
        let idle = number(&own, "IDLE_LAPSE_MINUTES");
        let inside = add(&creation, delta(py, "minutes", idle - 1.0));
        let outside = add(&creation, delta(py, "minutes", idle + 1.0));
        assert!(!overrun(&claim, &inside));
        assert!(active(&claim, &inside));
        assert!(!active(&claim, &outside));
        assert!(
            text(&claim.call_method1("lapse_reason", (&outside,)).unwrap())
                .contains("idle since creation")
        );
    });
}

#[test]
fn overrunning_shrinks_the_idle_window() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, 15.0, cap(&own), "alpha", "pkg/mod.py").unwrap();
        let wrote_at = add(
            &claim.getattr("creation").unwrap(),
            delta(py, "minutes", 12.0),
        );
        let id = text(&claim.getattr("claim_id").unwrap());
        touch(py, &own, &repo, &id, &wrote_at);
        let refreshed = only(py, &own, &repo);
        let overrun_idle = number(&own, "OVERRUN_IDLE_MINUTES");
        let on_time = add(&wrote_at, delta(py, "minutes", overrun_idle - 10.0));
        let overrun_at = add(&wrote_at, delta(py, "minutes", overrun_idle + 1.0));
        assert!(active(&refreshed, &on_time));
        assert!(!overrun(&refreshed, &on_time));
        assert!(overrun(&refreshed, &overrun_at));
        assert!(!active(&refreshed, &overrun_at));
        assert!(add(
            &wrote_at,
            delta(py, "minutes", number(&own, "IDLE_LAPSE_MINUTES"))
        )
        .gt(&overrun_at)
        .unwrap());
    });
}

#[test]
fn a_busy_overrun_claim_still_holds() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, 15.0, cap(&own), "alpha", "pkg/mod.py").unwrap();
        let id = text(&claim.getattr("claim_id").unwrap());
        let mut instant = claim.getattr("creation").unwrap();
        let idle = number(&own, "OVERRUN_IDLE_MINUTES");
        for _ in 0..6 {
            instant = add(&instant, delta(py, "minutes", idle - 1.0));
            touch(py, &own, &repo, &id, &instant);
            assert!(active(&only(py, &own, &repo), &instant));
        }
        let refreshed = only(py, &own, &repo);
        assert!(overrun(&refreshed, &instant));
        assert!(active(&refreshed, &instant));
        assert!(!active(
            &refreshed,
            &add(&instant, delta(py, "minutes", idle + 1.0))
        ));
    });
}

#[test]
fn the_hard_cap_ends_even_a_busy_claim() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let id = legacy_claim(py, &own, &repo, 8.0);
        let claim = only(py, &own, &repo);
        let cap_at = add(
            &claim.getattr("creation").unwrap(),
            delta(py, "hours", number(&own, "MAX_ACTIVE_CLAIM_HOURS")),
        );
        let before = sub(&cap_at, &delta(py, "minutes", 1.0));
        touch(py, &own, &repo, &id, &before);
        let refreshed = only(py, &own, &repo);
        let beyond = add(&cap_at, delta(py, "minutes", 1.0));
        assert!(active(&refreshed, &before));
        assert!(refreshed.getattr("expiry").unwrap().gt(&beyond).unwrap());
        assert!(refreshed
            .call_method1("idle_deadline", (&beyond,))
            .unwrap()
            .gt(&beyond)
            .unwrap());
        assert!(!active(&refreshed, &beyond));
        assert!(
            text(&refreshed.call_method1("lapse_reason", (&beyond,)).unwrap())
                .contains("expired at")
        );
    });
}

#[test]
fn the_lapse_message_names_the_overrun() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, 15.0, cap(&own), "alpha", "pkg/mod.py").unwrap();
        let creation = claim.getattr("creation").unwrap();
        let id = text(&claim.getattr("claim_id").unwrap());
        touch(
            py,
            &own,
            &repo,
            &id,
            &add(&creation, delta(py, "minutes", 12.0)),
        );
        let reason = text(
            &only(py, &own, &repo)
                .call_method1(
                    "lapse_reason",
                    (add(&creation, delta(py, "minutes", 40.0)),),
                )
                .unwrap(),
        );
        assert!(reason.contains("overran its expected"));
        assert!(reason.contains(&format!(
            "lapses after {}m",
            number(&own, "OVERRUN_IDLE_MINUTES") as i64
        )));
    });
}

#[test]
fn a_claim_stored_without_an_expected_time_still_loads() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let id = legacy_claim(py, &own, &repo, 1.0);
        let claim = only(py, &own, &repo);
        assert_eq!(text(&claim.getattr("claim_id").unwrap()), id);
        assert!(claim.getattr("expected_at").unwrap().is_none());
        same(
            &claim.getattr("expected").unwrap(),
            &claim.getattr("hard_deadline").unwrap(),
        );
        assert!(!overrun(
            &claim,
            &add(
                &claim.getattr("creation").unwrap(),
                delta(py, "minutes", 1.0)
            )
        ));
    });
}

#[test]
fn a_legacy_claim_round_trips_without_gaining_a_null_field() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let id = legacy_claim(py, &own, &repo, 1.0);
        touch(py, &own, &repo, &id, &now(py));
        make(py, &own, &repo, 15.0, 60.0, "beta", "pkg/other.py").unwrap();
        let store: Value =
            serde_json::from_slice(&fs::read(store_path(py, &own, &repo)).unwrap()).unwrap();
        let legacy: Vec<&Value> = store["claims"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["claim_id"] == id)
            .collect();
        assert!(!legacy.is_empty());
        assert!(legacy[0].get("expected_at").is_none());
        let claims = own
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .get_item(0)
            .unwrap();
        assert!(claims
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .any(|claim| text(&claim.getattr("claim_id").unwrap()) == id));
    });
}

#[test]
fn a_legacy_overlong_claim_is_still_capped() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        legacy_claim(py, &own, &repo, 8.0);
        let claim = only(py, &own, &repo);
        let creation = claim.getattr("creation").unwrap();
        same(
            &sub(&claim.getattr("expiry").unwrap(), &creation),
            &delta(py, "hours", 8.0),
        );
        same(
            &claim.getattr("hard_deadline").unwrap(),
            &add(
                &creation,
                delta(py, "hours", number(&own, "MAX_ACTIVE_CLAIM_HOURS")),
            ),
        );
    });
}

#[test]
fn a_write_resets_the_idle_timer() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = make(py, &own, &repo, cap(&own), cap(&own), "alpha", "pkg/mod.py").unwrap();
        let creation = claim.getattr("creation").unwrap();
        let idle = number(&own, "IDLE_LAPSE_MINUTES");
        let worked_at = add(&creation, delta(py, "minutes", idle - 1.0));
        let lapse = add(&creation, delta(py, "minutes", idle + 1.0));
        let id = text(&claim.getattr("claim_id").unwrap());
        assert!(touch(py, &own, &repo, &id, &worked_at));
        let refreshed = only(py, &own, &repo);
        same(&refreshed.getattr("activity").unwrap(), &worked_at);
        assert!(active(&refreshed, &lapse));
        assert!(!active(
            &refreshed,
            &add(&worked_at, delta(py, "minutes", idle + 1.0))
        ));
    });
}

#[test]
fn touching_twice_in_quick_succession_writes_once() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = ordinary(py, &own, &repo);
        let first = add(
            &claim.getattr("creation").unwrap(),
            delta(py, "minutes", 5.0),
        );
        let id = text(&claim.getattr("claim_id").unwrap());
        assert!(touch(py, &own, &repo, &id, &first));
        assert!(!touch(
            py,
            &own,
            &repo,
            &id,
            &add(&first, delta(py, "seconds", 30.0))
        ));
        let seen = own
            .getattr("load_activity")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(
            text(&seen.get_item(&id).unwrap()),
            text(&first.call_method0("isoformat").unwrap())
        );
        assert!(touch(
            py,
            &own,
            &repo,
            &id,
            &add(&first, delta(py, "seconds", 90.0))
        ));
    });
}

#[test]
fn a_lapsed_claim_stops_holding_the_path() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let stale = ordinary(py, &own, &repo);
        let stale_id = text(&stale.getattr("claim_id").unwrap());
        let past = sub(
            &now(py),
            &delta(py, "minutes", number(&own, "IDLE_LAPSE_MINUTES") + 1.0),
        );
        touch(py, &own, &repo, &stale_id, &past);
        let taken = make(py, &own, &repo, 15.0, 60.0, "beta", "pkg/mod.py").unwrap();
        let taken_id = text(&taken.getattr("claim_id").unwrap());
        let claims = own
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .get_item(0)
            .unwrap();
        let ids: Vec<String> = claims
            .try_iter()
            .unwrap()
            .map(|claim| text(&claim.unwrap().getattr("claim_id").unwrap()))
            .collect();
        assert_eq!(ids, [taken_id]);
        let seen = own
            .getattr("load_activity")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert!(!seen.contains(&stale_id).unwrap());
    });
}

#[test]
fn a_live_claim_still_blocks_another_owner() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        ordinary(py, &own, &repo);
        let error = make(py, &own, &repo, 15.0, 60.0, "beta", "pkg/mod.py").unwrap_err();
        assert_error(
            py,
            error,
            &own.getattr("OwnershipError").unwrap(),
            "overlaps active claim",
        );
    });
}

#[test]
fn activity_never_enters_the_claim_store() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        let claim = ordinary(py, &own, &repo);
        let id = text(&claim.getattr("claim_id").unwrap());
        touch(
            py,
            &own,
            &repo,
            &id,
            &add(
                &claim.getattr("creation").unwrap(),
                delta(py, "minutes", 5.0),
            ),
        );
        let store: Value =
            serde_json::from_slice(&fs::read(store_path(py, &own, &repo)).unwrap()).unwrap();
        assert!(store["claims"][0].get("last_seen").is_none());
        assert_eq!(
            text(&only(py, &own, &repo).getattr("claim_id").unwrap()),
            id
        );
        assert!(activity_path(py, &own, &repo).is_file());
    });
}

#[test]
fn a_corrupt_activity_log_fails_loud() {
    let case = isolated_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let own = ownership(py);
        ordinary(py, &own, &repo);
        fs::write(
            activity_path(py, &own, &repo),
            r#"{"schema_version": 99, "seen": {}}"#,
        )
        .unwrap();
        let error = own
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &own.getattr("OwnershipError").unwrap(),
            "activity log schema version",
        );
    });
}
