//! What the equivalence probe ablates in a change, and which recorded tests drive it.
//!
//! The probe ablates the functions of a module and the methods of its classes whose
//! names do not start with `__`: the units `f` and `C.m`. A changed line touches the
//! unit it falls in, nested functions included, unless it is in the unit's docstring;
//! a unit whose body is only `...` or `pass` has nothing to ablate. A unit's drivers
//! are the tests a passed run of which executed one of the unit's own lines in a
//! fixture or test window, in the file's current content, as `test_record_untested`
//! counts them, less the tests no longer in the tree.

use crate::test_record_digest::blob_oid;
use crate::test_record_store::open_read_only;
use crate::test_record_untested::{functions, nested, Function};
use anyhow::Context;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The probe records only the first 24 calls of each function, so a few tests supply
/// them and more add only recording time, most of a probe's cost: the tests that ran
/// the most of the unit's own lines win, the fastest among equals.
const DRIVERS_PER_UNIT: usize = 8;

/// Whether the probe ablates the function named `qualname`.
fn probed(qualname: &str) -> bool {
    !qualname.contains("<locals>")
        && qualname
            .split_once('.')
            .is_none_or(|(_, method)| !method.contains('.') && !method.starts_with("__"))
}

/// A changed file, the oid of its content, its functions, and which of them are units.
struct File<'c> {
    path: &'c str,
    whole: bool,
    oid: String,
    functions: Vec<Function>,
    units: Vec<usize>,
}

/// `path` under `root`, with every unit a target when `changed` is `None`, else the
/// units a changed line touches.
fn file<'c>(root: &Path, path: &'c str, changed: Option<&[usize]>) -> anyhow::Result<File<'c>> {
    let bytes = std::fs::read(root.join(path)).with_context(|| path.to_owned())?;
    let oid = blob_oid(&bytes);
    let source = String::from_utf8(bytes).with_context(|| path.to_owned())?;
    let functions = functions(&source).with_context(|| path.to_owned())?;
    let units = (0..functions.len())
        .filter(|&index| {
            let function = &functions[index];
            probed(&function.qualname)
                && function.runs.is_some()
                && changed.is_none_or(|lines| lines.iter().any(|line| function.owns(*line, &[])))
        })
        .collect();
    Ok(File {
        path,
        whole: changed.is_none(),
        oid,
        functions,
        units,
    })
}

/// A unit's `(file, unit)` position and the own lines of it one window executed.
type Hit = (usize, usize, Vec<usize>);

/// Per fixture or test window that executed a unit's own lines in its file's current
/// content, those units and lines.
fn windows(connection: &Connection, files: &[File]) -> anyhow::Result<HashMap<String, Vec<Hit>>> {
    let by_path: HashMap<&str, usize> = (0..files.len()).map(|at| (files[at].path, at)).collect();
    let mut hits = HashMap::new();
    let mut statement = connection
        .prepare("SELECT digest, lines FROM windows WHERE kind IN ('fixture', 'test')")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(1)?;
        let entries: Vec<(String, String, Vec<usize>)> =
            serde_json::from_slice(&zstd::decode_all(blob.as_slice())?)?;
        let mut hit = Vec::new();
        for (label, oid, lines) in entries {
            let Some(&at) = by_path.get(label.as_str()) else {
                continue;
            };
            let file = &files[at];
            if file.oid != oid {
                continue;
            }
            for (slot, &index) in file.units.iter().enumerate() {
                let function = &file.functions[index];
                let inner = nested(&file.functions, index);
                let owned: Vec<usize> = lines
                    .iter()
                    .copied()
                    .filter(|&line| {
                        function.runs.is_some_and(|runs| line >= runs) && function.owns(line, inner)
                    })
                    .collect();
                if !owned.is_empty() {
                    hit.push((at, slot, owned));
                }
            }
        }
        if !hit.is_empty() {
            hits.insert(row.get::<_, String>(0)?, hit);
        }
    }
    Ok(hits)
}

/// Which recorded tests are still in `root`: the nodeid's file is there and defines
/// the function it names. A gone file defines none.
struct Tests<'r> {
    root: &'r Path,
    defined: HashMap<String, HashSet<String>>,
}

impl Tests<'_> {
    fn exist(&mut self, nodeid: &str) -> anyhow::Result<bool> {
        let named = nodeid.split_once('[').map_or(nodeid, |(head, _)| head);
        let Some((path, test)) = named.split_once("::") else {
            return Ok(false);
        };
        if !self.defined.contains_key(path) {
            let found: HashSet<String> = match std::fs::read_to_string(self.root.join(path)) {
                Ok(source) => functions(&source)
                    .with_context(|| path.to_owned())?
                    .into_iter()
                    .map(|function| function.qualname)
                    .collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
                Err(error) => return Err(error).with_context(|| path.to_owned()),
            };
            self.defined.insert(path.to_owned(), found);
        }
        Ok(self.defined[path].contains(&test.replace("::", ".")))
    }
}

/// Per test, its fastest passed run's seconds and the unit's own lines its runs executed.
type Driven = HashMap<String, (f64, HashSet<usize>)>;

/// The nodeids of the strongest of `runs` still in the tree, sorted.
fn strongest(runs: Driven, tests: &mut Tests) -> anyhow::Result<Vec<String>> {
    let mut ranked: Vec<(String, (f64, HashSet<usize>))> = runs.into_iter().collect();
    ranked.sort_by(|(a, (a_took, a_ran)), (b, (b_took, b_ran))| {
        b_ran
            .len()
            .cmp(&a_ran.len())
            .then_with(|| a_took.total_cmp(b_took))
            .then_with(|| a.cmp(b))
    });
    let mut chosen = Vec::new();
    for (nodeid, _) in ranked {
        if chosen.len() == DRIVERS_PER_UNIT {
            break;
        }
        if tests.exist(&nodeid)? {
            chosen.push(nodeid);
        }
    }
    chosen.sort();
    Ok(chosen)
}

/// Per file and unit, its drivers' nodeids.
fn drivers(root: &Path, store: &Path, files: &[File]) -> anyhow::Result<Vec<Vec<Vec<String>>>> {
    let mut driven: Vec<Vec<Driven>> = files
        .iter()
        .map(|file| vec![HashMap::new(); file.units.len()])
        .collect();
    if let Some(connection) = open_read_only(store)? {
        let hits = windows(&connection, files)?;
        let mut statement = connection.prepare(
            "SELECT r.nodeid, r.seconds, w.window FROM runs r \
             JOIN run_windows w ON w.run = r.id WHERE r.outcome = 'passed'",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let Some(hit) = hits.get(&row.get::<_, String>(2)?) else {
                continue;
            };
            let nodeid: String = row.get(0)?;
            let took: f64 = row.get(1)?;
            for (at, slot, lines) in hit {
                let (best, ran) = driven[*at][*slot]
                    .entry(nodeid.clone())
                    .or_insert_with(|| (took, HashSet::new()));
                *best = best.min(took);
                ran.extend(lines);
            }
        }
    }
    let mut tests = Tests {
        root,
        defined: HashMap::new(),
    };
    driven
        .into_iter()
        .map(|units| {
            units
                .into_iter()
                .map(|runs| strongest(runs, &mut tests))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .collect()
}

/// A unit the probe ablates: `(qualname, def line, driver nodeids)`.
type Unit = (String, usize, Vec<String>);

/// Per change, `(path, whole, units)`.
fn targets(
    root: &Path,
    store: &Path,
    changes: &[(String, Option<Vec<usize>>)],
) -> anyhow::Result<Vec<(String, bool, Vec<Unit>)>> {
    let files = changes
        .iter()
        .map(|(path, changed)| file(root, path, changed.as_deref()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let drivers = drivers(root, store, &files)?;
    Ok(files
        .iter()
        .zip(drivers)
        .map(|(file, drivers)| {
            let units = file
                .units
                .iter()
                .zip(drivers)
                .map(|(&index, nodeids)| {
                    let function = &file.functions[index];
                    (function.qualname.clone(), function.line, nodeids)
                })
                .collect();
            (file.path.to_owned(), file.whole, units)
        })
        .collect())
}

/// For `changes`, each a path relative to `root` and its changed line numbers, or
/// `None` for the whole file: per change `(path, whole, units)`, each unit the
/// `(qualname, def line, driver nodeids)` of one the probe ablates, its drivers from
/// the runs recorded in `store`.
#[pyfunction]
pub(crate) fn equivalence_targets_native(
    py: Python<'_>,
    root: &str,
    store: &str,
    changes: Vec<(String, Option<Vec<usize>>)>,
) -> PyResult<Vec<(String, bool, Vec<Unit>)>> {
    py.detach(|| targets(Path::new(root), Path::new(store), &changes))
        .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_record_store::{self as store, RunRow, SessionRow, WindowRow};
    use serde_json::{json, Value};

    const SOURCE: &str = r#"def plain(x):
    """Doc."""
    return x + 1


def outer():
    def inner():
        return 2

    return inner


class C:
    def method(self):
        return 3

    def __init__(self):
        self.x = 4

    def stub(self):
        ...


def untouched():
    return 5
"#;

    const TESTS: &str =
        "def test_a():\n    pass\n\n\nclass TestB:\n    def test_b(self):\n        pass\n";

    fn window(digest: &str, kind: &'static str, lines: Value) -> WindowRow {
        WindowRow {
            digest: digest.to_owned(),
            kind,
            key: String::new(),
            opaque: None,
            deps: json!([]),
            lines,
        }
    }

    fn run(nodeid: &str, outcome: &str, seconds: f64, windows: &[&str]) -> RunRow {
        RunRow {
            nodeid: nodeid.to_owned(),
            path: nodeid.split("::").next().unwrap().to_owned(),
            outcome: outcome.to_owned(),
            opaque: None,
            seconds,
            windows: windows.iter().map(|digest| (*digest).to_owned()).collect(),
        }
    }

    #[test]
    fn a_unit_is_driven_by_the_passed_tests_that_ran_its_own_lines() {
        let root = std::env::temp_dir().join(format!("equivalence-targets-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.py"), SOURCE).unwrap();
        std::fs::write(root.join("t.py"), TESTS).unwrap();
        let db = root.join("record.sqlite");
        let oid = blob_oid(SOURCE.as_bytes());
        let session = SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".to_owned(),
            env_digest: "env".to_owned(),
            env_data: json!({}),
            argv: "[]".to_owned(),
            unstable: 0,
        };
        store::write(
            &db,
            &session,
            &[
                // Line 8 is inner's body, not outer's.
                window("inner", "test", json!([["m.py", oid, [8]]])),
                window("both", "test", json!([["m.py", oid, [10, 15]]])),
                window("plain", "fixture", json!([["m.py", oid, [3]]])),
                window("old", "test", json!([["m.py", blob_oid(b"old"), [3, 15]]])),
                window("collect", "collect", json!([["m.py", oid, [1, 3, 6, 25]]])),
            ],
            &[
                run("t.py::test_a", "passed", 0.5, &["both", "inner", "old"]),
                run("t.py::TestB::test_b", "passed", 0.1, &["both", "plain"]),
                run("t.py::test_a[x-1]", "passed", 0.2, &["plain"]),
                run("t.py::test_a", "failed", 0.2, &["plain"]),
                run("t.py::test_gone", "passed", 0.01, &["both"]),
                run("gone.py::test_x", "passed", 0.01, &["both"]),
                run("t.py::test_c", "passed", 0.01, &["collect"]),
            ],
        )
        .unwrap();
        let both = vec!["t.py::TestB::test_b".to_owned(), "t.py::test_a".to_owned()];
        let unit =
            |name: &str, line: usize, nodeids: &[String]| (name.to_owned(), line, nodeids.to_vec());
        // The docstring (2), inner's body (8), the method (15), a dunder (18) and the
        // stub (21).
        let changes = [("m.py".to_owned(), Some(vec![2, 8, 15, 18, 21]))];
        assert_eq!(
            targets(&root, &db, &changes).unwrap(),
            vec![(
                "m.py".to_owned(),
                false,
                vec![unit("outer", 6, &both), unit("C.method", 14, &both)]
            )]
        );
        let whole = [("m.py".to_owned(), None)];
        let plain = [
            "t.py::TestB::test_b".to_owned(),
            "t.py::test_a[x-1]".to_owned(),
        ];
        assert_eq!(
            targets(&root, &db, &whole).unwrap(),
            vec![(
                "m.py".to_owned(),
                true,
                vec![
                    unit("plain", 1, &plain),
                    unit("outer", 6, &both),
                    unit("C.method", 14, &both),
                    unit("untouched", 24, &[]),
                ]
            )]
        );
        // With no store, no unit has drivers.
        assert_eq!(
            targets(&root, &root.join("none.sqlite"), &changes).unwrap(),
            vec![(
                "m.py".to_owned(),
                false,
                vec![unit("outer", 6, &[]), unit("C.method", 14, &[])]
            )]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_unit_keeps_the_drivers_that_ran_most_of_it_fastest() {
        let root =
            std::env::temp_dir().join(format!("equivalence-strongest-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source: String = (0..10)
            .map(|n| format!("def test_{n}():\n    pass\n"))
            .collect();
        std::fs::write(root.join("t.py"), source).unwrap();
        let mut tests = Tests {
            root: &root,
            defined: HashMap::new(),
        };
        // The two slowest ran two of the unit's lines, the rest one, the gone test three.
        let ran = |count: usize| (0..count).collect::<HashSet<usize>>();
        let runs = (0..10)
            .map(|n| {
                let lines = if n < 2 { 2 } else { 1 };
                (format!("t.py::test_{n}"), (f64::from(10 - n), ran(lines)))
            })
            .chain([("t.py::test_gone".to_owned(), (0.0, ran(3)))])
            .collect();
        let kept: Vec<String> = [0, 1, 4, 5, 6, 7, 8, 9]
            .map(|n| format!("t.py::test_{n}"))
            .to_vec();
        assert_eq!(strongest(runs, &mut tests).unwrap(), kept);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
