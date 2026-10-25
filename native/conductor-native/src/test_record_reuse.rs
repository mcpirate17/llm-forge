//! Reuse: which collected tests have a recorded pass against the inputs they would
//! depend on now.
//!
//! A test's newest run under the same environment key whose every recorded
//! dependency still has its recorded value decides: it is reused when that run
//! passed and nothing in it was opaque. A newer identical-input failure therefore
//! outranks an older pass. Directory listings are compared in fixture and test
//! windows, where a test globbed a directory; in session and collect windows they
//! are the import system scanning package directories, so a new file there does not
//! invalidate every importer. A test that would see the new file through a changed
//! import is caught by that import's own file.

use crate::test_record_paths::Places;
use crate::test_record_state::{Dep, DepKind};
use crate::test_record_store::open_read_only;
use rusqlite::params;
use std::collections::HashMap;
use std::path::Path;

/// Runs of one test considered, newest first, before giving up on finding one whose
/// inputs match.
const RUNS_PER_TEST: i64 = 64;

/// The recorded pass a test is skipped on.
#[derive(Debug, PartialEq)]
pub(crate) struct Reused {
    pub(crate) session: i64,
    /// The recorded run's duration: the time the skip saves.
    pub(crate) seconds: f64,
}

struct Checker<'a> {
    places: &'a Places,
    values: HashMap<(DepKind, String), String>,
    windows: HashMap<String, bool>,
}

impl Checker<'_> {
    fn current(&mut self, kind: DepKind, label: &str) -> String {
        if let Some(value) = self.values.get(&(kind, label.to_owned())) {
            return value.clone();
        }
        let found = match kind {
            DepKind::Installed => Some(self.places.dist(label)),
            DepKind::File | DepKind::Listing => self.places.resolve(label),
        };
        let value = found.map_or_else(String::new, |(place, real)| {
            Dep {
                kind,
                place,
                real,
                first: String::new(),
            }
            .current()
        });
        self.values.insert((kind, label.to_owned()), value.clone());
        value
    }

    /// Whether every dependency the window recorded still has its recorded value.
    fn matches(&mut self, kind: &str, deps: &[u8]) -> anyhow::Result<bool> {
        let compare_listings = matches!(kind, "fixture" | "test");
        let deps: Vec<(String, String, String)> = serde_json::from_slice(&zstd::decode_all(deps)?)?;
        for (dep_kind, label, value) in &deps {
            let dep_kind = match dep_kind.as_str() {
                "file" => DepKind::File,
                "list" if compare_listings => DepKind::Listing,
                "list" => continue,
                "inst" => DepKind::Installed,
                other => anyhow::bail!("unknown dependency kind {other:?}"),
            };
            if self.current(dep_kind, label) != *value {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `(matches, opaque)` over every window of one run.
    fn run(
        &mut self,
        windows: &mut rusqlite::Statement<'_>,
        run: i64,
    ) -> anyhow::Result<(bool, bool)> {
        let rows = windows
            .query_map(params![run], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut opaque = false;
        for (digest, kind, window_opaque, deps) in rows {
            opaque |= window_opaque.is_some();
            let matched = match self.windows.get(&digest) {
                Some(known) => *known,
                None => {
                    let matched = self.matches(&kind, &deps)?;
                    self.windows.insert(digest, matched);
                    matched
                }
            };
            if !matched {
                return Ok((false, opaque));
            }
        }
        Ok((true, opaque))
    }
}

/// For each `(nodeid, path)` pytest collected, the recorded pass it can be skipped
/// on, if any. A missing store reuses nothing; a store of another schema is an error.
pub(crate) fn reusable(
    store: &Path,
    environment: &str,
    places: &Places,
    tests: &[(String, String)],
) -> anyhow::Result<Vec<Option<Reused>>> {
    let Some(connection) = open_read_only(store)? else {
        return Ok(tests.iter().map(|_| None).collect());
    };
    let mut runs = connection.prepare(
        "SELECT r.id, r.session, r.outcome, r.opaque, r.seconds FROM runs r \
         JOIN sessions s ON s.id = r.session \
         WHERE r.nodeid = ?1 AND s.env = ?2 ORDER BY r.id DESC LIMIT ?3",
    )?;
    let mut windows = connection.prepare(
        "SELECT w.digest, w.kind, w.opaque, w.deps FROM run_windows rw \
         JOIN windows w ON w.digest = rw.window WHERE rw.run = ?1",
    )?;
    let mut checker = Checker {
        places,
        values: HashMap::new(),
        windows: HashMap::new(),
    };
    let mut out = Vec::with_capacity(tests.len());
    for (nodeid, path) in tests {
        let (canonical, _) = places.rerooted(nodeid, Path::new(path));
        let candidates = runs
            .query_map(params![canonical, environment, RUNS_PER_TEST], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut decided = None;
        for (run, session, outcome, run_opaque, seconds) in candidates {
            let (matched, window_opaque) = checker.run(&mut windows, run)?;
            if matched {
                decided = (outcome == "passed" && run_opaque.is_none() && !window_opaque)
                    .then_some(Reused { session, seconds });
                break;
            }
        }
        out.push(decided);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_record_digest::blob_oid;
    use crate::test_record_store::{self as store, RunRow, SessionRow, WindowRow};
    use serde_json::{json, Value};

    fn window(digest: &str, kind: &'static str, deps: Value) -> WindowRow {
        WindowRow {
            digest: digest.to_owned(),
            kind,
            key: String::new(),
            opaque: None,
            deps,
            lines: json!([]),
        }
    }

    fn run(nodeid: &str, outcome: &str, windows: &[&str]) -> RunRow {
        RunRow {
            nodeid: nodeid.to_owned(),
            path: "t.py".to_owned(),
            outcome: outcome.to_owned(),
            opaque: None,
            seconds: 2.5,
            windows: windows.iter().map(|w| (*w).to_owned()).collect(),
        }
    }

    fn session(env: &str) -> SessionRow {
        SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".to_owned(),
            env_digest: env.to_owned(),
            env_data: json!({}),
            argv: "[]".to_owned(),
            unstable: 0,
        }
    }

    #[test]
    fn the_newest_run_with_matching_inputs_decides() {
        let root = std::env::temp_dir().join(format!("test-record-reuse-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(root.join("a.py"), "a = 1\n").unwrap();
        std::fs::write(root.join("pkg/b.py"), "").unwrap();
        let places = Places::new(root.to_str().unwrap(), &[], &[], None);
        let db = root.join("record.sqlite");
        let a = blob_oid(b"a = 1\n");
        let old = blob_oid(b"a = 0\n");
        let listing = crate::test_record_digest::listing(&root.join("pkg")).unwrap();
        let stale_listing = json!(["list", "pkg", "list:stale"]);
        store::write(
            &db,
            &session("env"),
            &[
                window("s", "session", json!([["file", "a.py", a], stale_listing])),
                window(
                    "t-new",
                    "test",
                    json!([["file", "a.py", a], ["list", "pkg", listing]]),
                ),
                window("t-old", "test", json!([["file", "a.py", old]])),
                window(
                    "t-glob",
                    "test",
                    json!([["file", "a.py", a], stale_listing]),
                ),
            ],
            &[
                run("t.py::passes", "passed", &["s", "t-new"]),
                run("t.py::passed_on_old_code", "passed", &["s", "t-old"]),
                run("t.py::globbed", "passed", &["s", "t-glob"]),
                run("t.py::flaky", "passed", &["s", "t-new"]),
            ],
        )
        .unwrap();
        // A later session saw the flaky test fail against the same inputs: the same
        // windows, already stored.
        store::write(
            &db,
            &session("env"),
            &[],
            &[run("t.py::flaky", "failed", &["s", "t-new"])],
        )
        .unwrap();
        let tests: Vec<(String, String)> = [
            "passes",
            "passed_on_old_code",
            "globbed",
            "flaky",
            "never_ran",
        ]
        .iter()
        .map(|name| {
            (
                format!("t.py::{name}"),
                root.join("t.py").to_string_lossy().into(),
            )
        })
        .collect();
        let found = reusable(&db, "env", &places, &tests).unwrap();
        assert_eq!(
            found,
            [
                Some(Reused {
                    session: 1,
                    seconds: 2.5
                }),
                None,
                None,
                None,
                None
            ]
        );
        // Another environment key reuses nothing.
        assert!(reusable(&db, "other", &places, &tests)
            .unwrap()
            .iter()
            .all(Option::is_none));
        // A missing store reuses nothing.
        let missing = reusable(&root.join("none.sqlite"), "env", &places, &tests).unwrap();
        assert!(missing.iter().all(Option::is_none));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_rebuilt_distribution_reruns_only_the_tests_that_loaded_it() {
        let root =
            std::env::temp_dir().join(format!("test-record-reuse-inst-{}", std::process::id()));
        let site = root.join("site");
        for (label, file) in [
            ("alpha-1.0.dist-info", "alpha.py"),
            ("beta-1.0.dist-info", "beta.py"),
        ] {
            std::fs::create_dir_all(site.join(label)).unwrap();
            std::fs::write(
                site.join(label).join("RECORD"),
                format!("{file},sha256=v1,1\n"),
            )
            .unwrap();
        }
        let sites = [site.to_string_lossy().into_owned()];
        let places = Places::new(root.to_str().unwrap(), &[], &[], None)
            .with_installed(crate::test_record_installed::Installed::new(&sites).unwrap());
        let value = |label: &str| crate::test_record_digest::distribution(&site.join(label));
        let db = root.join("record.sqlite");
        store::write(
            &db,
            &session("env"),
            &[
                window(
                    "s",
                    "session",
                    json!([["inst", "beta-1.0.dist-info", value("beta-1.0.dist-info")]]),
                ),
                window(
                    "t-alpha",
                    "test",
                    json!([["inst", "alpha-1.0.dist-info", value("alpha-1.0.dist-info")]]),
                ),
                window("t-none", "test", json!([])),
            ],
            &[
                run("t.py::uses_alpha", "passed", &["s", "t-alpha"]),
                run("t.py::uses_neither", "passed", &["s", "t-none"]),
            ],
        )
        .unwrap();
        let tests: Vec<(String, String)> = ["uses_alpha", "uses_neither"]
            .iter()
            .map(|name| {
                (
                    format!("t.py::{name}"),
                    root.join("t.py").to_string_lossy().into(),
                )
            })
            .collect();
        let reused = |places: &Places| {
            reusable(&db, "env", places, &tests)
                .unwrap()
                .iter()
                .map(Option::is_some)
                .collect::<Vec<_>>()
        };
        assert_eq!(reused(&places), [true, true]);
        std::fs::write(
            site.join("alpha-1.0.dist-info/RECORD"),
            "alpha.py,sha256=v2,1\n",
        )
        .unwrap();
        assert_eq!(reused(&places), [false, true]);
        // A version bump renames the metadata directory: the recorded one is absent.
        std::fs::rename(
            site.join("beta-1.0.dist-info"),
            site.join("beta-2.0.dist-info"),
        )
        .unwrap();
        let bumped = Places::new(root.to_str().unwrap(), &[], &[], None)
            .with_installed(crate::test_record_installed::Installed::new(&sites).unwrap());
        assert_eq!(reused(&bumped), [false, false]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
