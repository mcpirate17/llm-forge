//! Session end: the static import closure, the environment key, the stability
//! check, and the rows the store writes.

use crate::graph_index;
use crate::test_record_digest::{sha1_hex, EDITABLE};
use crate::test_record_installed::Installed;
use crate::test_record_paths::Place;
use crate::test_record_state::{bit_lines, Dep, DepKind, Kind, Run, State, Window};
use crate::test_record_store::{self as store, RunRow, SessionRow, WindowRow};
use serde_json::{json, Value};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// What the interpreter knows at session end, read before the state lock is taken.
pub(crate) struct Snapshot {
    /// `(name, __file__)` of every loaded module that has a file.
    pub(crate) modules: Vec<(String, String)>,
    pub(crate) inipath: Option<String>,
}

/// The environment key: a record is reusable only under an equal key. It names the
/// installed distributions without their versions: a test records the ones it
/// loaded, and an optional import that was absent changes the key when installed.
pub(crate) struct Environment {
    pub(crate) python: String,
    pub(crate) vars: BTreeMap<String, Option<String>>,
    pub(crate) site_dirs: Vec<String>,
    pub(crate) installed: BTreeSet<String>,
}

impl Environment {
    fn value(&self) -> Value {
        json!({"python": self.python, "installed": self.installed, "vars": self.vars})
    }

    pub(crate) fn digest(&self) -> String {
        sha1_hex(self.value().to_string().as_bytes())
    }
}

/// Loaded modules by name, and by real path for resolving relative imports.
struct Modules {
    files: HashMap<String, (Place, PathBuf)>,
    names: HashMap<PathBuf, String>,
}

impl Modules {
    fn new(state: &mut State, snapshot: &Snapshot) -> Self {
        let mut files = HashMap::new();
        let mut names = HashMap::new();
        for (name, file) in &snapshot.modules {
            if let Some((place, real)) = state.locate(Path::new(file)) {
                names.insert(real.clone(), name.clone());
                files.insert(name.clone(), (place, real));
            }
        }
        Self { files, names }
    }

    /// The package relative imports in `path` resolve against.
    fn package_of(&self, path: &Path) -> Option<String> {
        let name = self.names.get(path)?;
        if path.file_name().is_some_and(|file| file == "__init__.py") {
            return Some(name.clone());
        }
        name.rsplit_once('.').map(|(package, _)| package.to_owned())
    }
}

fn with_parents(name: &str, out: &mut BTreeSet<String>) {
    let mut current = name;
    loop {
        out.insert(current.to_owned());
        match current.rsplit_once('.') {
            Some((parent, _)) => current = parent,
            None => return,
        }
    }
}

/// `module` as graph_index records it: leading dots mark a relative import.
fn absolute(module: &str, package: Option<&str>) -> Option<String> {
    let rest = module.trim_start_matches('.');
    let level = module.len() - rest.len();
    if level == 0 {
        return (!rest.is_empty()).then(|| rest.to_owned());
    }
    let parts: Vec<&str> = package?.split('.').collect();
    let keep = parts.len().checked_sub(level - 1)?;
    let mut base = parts[..keep].join(".");
    if !rest.is_empty() {
        if !base.is_empty() {
            base.push('.');
        }
        base.push_str(rest);
    }
    (!base.is_empty()).then_some(base)
}

/// Every module an executed file imports directly, its parent packages, and the
/// file's own parent packages. Static: an import inside an unexecuted branch still
/// counts, and an import the test only relied on through `sys.modules` is found.
fn imported_names(path: &Path, modules: &Modules) -> anyhow::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    if let Some(own) = modules.names.get(path) {
        with_parents(own, &mut names);
    }
    if path.extension().is_none_or(|ext| ext != "py") {
        return Ok(names);
    }
    let Ok(source) = std::fs::read_to_string(path) else {
        return Ok(names);
    };
    let facts = graph_index::extract_python(&source)
        .map_err(|error| anyhow::anyhow!("{}: {}", path.display(), error.0))?;
    let package = modules.package_of(path);
    for import in &facts.imports {
        let Some(base) = absolute(&import.module, package.as_deref()) else {
            continue;
        };
        if let Some(member) = &import.member {
            names.insert(format!("{base}.{member}"));
        }
        with_parents(&base, &mut names);
    }
    for literal in &facts.literal_imports {
        with_parents(literal, &mut names);
    }
    Ok(names)
}

#[derive(Default)]
struct Closure {
    cache: HashMap<u32, Vec<u32>>,
}

impl Closure {
    fn extend(
        &mut self,
        state: &mut State,
        modules: &Modules,
        window: &mut Window,
    ) -> anyhow::Result<()> {
        let executed: Vec<u32> = window.executed.keys().copied().collect();
        for id in executed {
            let found = match self.cache.entry(id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let path = state.deps[id as usize].real.clone();
                    let mut found = Vec::new();
                    for name in imported_names(&path, modules)? {
                        if let Some((place, real)) = modules.files.get(&name) {
                            found.push(state.dep(DepKind::File, place.clone(), real.clone()));
                        }
                    }
                    entry.insert(found)
                }
            };
            window.deps.extend(found.iter().copied());
        }
        Ok(())
    }
}

/// Installed distributions bring the ones they require: native code loads its
/// libraries through the dynamic linker, which no audit event sees. An editable
/// install is repository source, whose imports the closure already follows.
#[derive(Default)]
struct Requirements {
    cache: HashMap<u32, Vec<u32>>,
}

impl Requirements {
    fn extend(&mut self, state: &mut State, window: &mut Window) {
        let mut pending: Vec<u32> = window
            .deps
            .iter()
            .copied()
            .filter(|id| state.deps[*id as usize].kind == DepKind::Installed)
            .collect();
        while let Some(id) = pending.pop() {
            let required = match self.cache.entry(id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let dep = &state.deps[id as usize];
                    let labels = if dep.first == EDITABLE {
                        Vec::new()
                    } else {
                        state.places.installed().requires(&dep.place.label())
                    };
                    let found = labels
                        .iter()
                        .map(|label| {
                            let (place, real) = state.places.dist(label);
                            state.dep(DepKind::Installed, place, real)
                        })
                        .collect();
                    entry.insert(found)
                }
            };
            pending.extend(
                required
                    .iter()
                    .copied()
                    .filter(|id| window.deps.insert(*id)),
            );
        }
    }
}

/// Session-wide dependencies pytest and the interpreter load outside any window:
/// loaded extension modules and the ini file. An extension of a distribution some
/// window already depends on is that window's; one no window names is everyone's.
fn session_extras(
    state: &mut State,
    snapshot: &Snapshot,
    session: &mut Window,
    held: &BTreeSet<u32>,
) {
    let extensions = snapshot
        .modules
        .iter()
        .map(|(_, file)| file.as_str())
        .filter(|file| file.ends_with(".so") || file.ends_with(".pyd"));
    for file in extensions.chain(snapshot.inipath.as_deref()) {
        let Some((place, _)) = state.locate(Path::new(file)) else {
            continue;
        };
        if state
            .known(DepKind::Installed, &place)
            .is_some_and(|id| held.contains(&id))
        {
            continue;
        }
        if let Some((id, _)) = state.located(DepKind::File, Path::new(file)) {
            session.deps.insert(id);
        }
    }
}

/// The session window and the shared windows, after the static additions.
struct Windows {
    session: Window,
    shared: HashMap<(Kind, String), Window>,
}

fn complete(state: &mut State, snapshot: &Snapshot, runs: &mut [Run]) -> anyhow::Result<Windows> {
    let (_, _, mut session) = state.stack.pop().expect("one window is open");
    let mut shared = std::mem::take(&mut state.closed);
    let modules = Modules::new(state, snapshot);
    let mut closure = Closure::default();
    let mut requirements = Requirements::default();
    let mut held = BTreeSet::new();
    let windows = std::iter::once(&mut session)
        .chain(shared.values_mut())
        .chain(runs.iter_mut().map(|run| &mut run.window));
    for window in windows {
        closure.extend(state, &modules, window)?;
        requirements.extend(state, window);
        held.extend(
            window
                .deps
                .iter()
                .copied()
                .filter(|id| state.deps[*id as usize].kind == DepKind::Installed),
        );
    }
    session_extras(state, snapshot, &mut session, &held);
    requirements.extend(state, &mut session);
    Ok(Windows { session, shared })
}

struct Rows<'a> {
    state: &'a State,
    last: Vec<String>,
    rows: BTreeMap<String, WindowRow>,
    shared: HashMap<(Kind, String), Option<String>>,
    /// Labels of the dependencies that changed during the session.
    changed: BTreeSet<String>,
}

impl Rows<'_> {
    /// The window's digest, or `None` when a dependency changed during the session.
    fn add(&mut self, kind: Kind, key: &str, window: &Window) -> Option<String> {
        let mut deps = Vec::new();
        let mut stable = true;
        for id in window.all_deps() {
            let dep = &self.state.deps[id as usize];
            let last = &self.last[id as usize];
            if *last != dep.first {
                self.changed.insert(dep.place.label());
                stable = false;
            }
            deps.push(json!([dep.kind.name(), dep.place.label(), last]));
        }
        if !stable {
            return None;
        }
        deps.sort_by_key(Value::to_string);
        let lines: Vec<Value> = window
            .executed
            .iter()
            .map(|(id, bits)| {
                let dep = &self.state.deps[*id as usize];
                json!([dep.place.label(), self.last[*id as usize], bit_lines(bits)])
            })
            .collect();
        let canonical = json!([kind.name(), key, window.opaque, deps, lines]);
        let digest = sha1_hex(canonical.to_string().as_bytes());
        self.rows
            .entry(digest.clone())
            .or_insert_with(|| WindowRow {
                digest: digest.clone(),
                kind: kind.name(),
                key: key.to_owned(),
                opaque: window.opaque.clone(),
                deps: Value::Array(deps),
                lines: Value::Array(lines),
            });
        Some(digest)
    }

    fn shared(&mut self, kind: Kind, key: &str, window: &Window) -> Option<String> {
        let id = (kind, key.to_owned());
        if let Some(known) = self.shared.get(&id) {
            return known.clone();
        }
        let digest = self.add(kind, key, window);
        self.shared.insert(id, digest.clone());
        digest
    }
}

/// The runs to store and the windows they reference. A run that depends on a file
/// that changed during the session is dropped and counted, and the file is named.
fn rows(
    state: &State,
    windows: &Windows,
    runs: &[Run],
) -> (Vec<RunRow>, Vec<WindowRow>, usize, BTreeSet<String>) {
    let last = state.deps.iter().map(Dep::current).collect();
    let mut rows = Rows {
        state,
        last,
        rows: BTreeMap::new(),
        shared: HashMap::new(),
        changed: BTreeSet::new(),
    };
    let session = rows.add(Kind::Session, "", &windows.session);
    let mut stored = Vec::new();
    let mut unstable = 0;
    for run in runs {
        let mut digests = vec![session.clone()];
        let mut opaque = run.window.opaque.clone();
        let used = run
            .module
            .iter()
            .map(|key| (Kind::Collect, key))
            .chain(run.fixtures.iter().map(|key| (Kind::Fixture, key)));
        for (kind, key) in used {
            let Some(window) = windows.shared.get(&(kind, key.clone())) else {
                continue;
            };
            digests.push(rows.shared(kind, key, window));
            if kind == Kind::Fixture && opaque.is_none() {
                opaque = window.opaque.clone();
            }
        }
        digests.push(rows.add(Kind::Test, &run.nodeid, &run.window));
        match digests.into_iter().collect::<Option<Vec<String>>>() {
            Some(windows) => stored.push(RunRow {
                nodeid: run.nodeid.clone(),
                path: run.path.clone(),
                outcome: run.outcome.to_owned(),
                opaque,
                seconds: run.seconds,
                windows,
            }),
            None => unstable += 1,
        }
    }
    let referenced: BTreeSet<&String> = stored.iter().flat_map(|run| &run.windows).collect();
    let window_rows = rows
        .rows
        .into_values()
        .filter(|row| referenced.contains(&row.digest))
        .collect();
    (stored, window_rows, unstable, rows.changed)
}

pub(crate) struct Session<'a> {
    pub(crate) store: &'a Path,
    pub(crate) started: f64,
    pub(crate) argv: &'a str,
    pub(crate) environment: &'a Environment,
    pub(crate) snapshot: &'a Snapshot,
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

pub(crate) fn finish(mut state: State, session: &Session<'_>) -> anyhow::Result<String> {
    if let Some(failure) = state.failure.take() {
        anyhow::bail!("{failure}");
    }
    if state.stack.len() != 1 {
        let open: Vec<&str> = state.stack[1..]
            .iter()
            .map(|(_, key, _)| key.as_str())
            .collect();
        anyhow::bail!("windows still open at session end: {open:?}");
    }
    let mut runs = std::mem::take(&mut state.runs);
    if runs.is_empty() {
        return Ok("no tests ran; nothing recorded".to_owned());
    }
    if Installed::names(&session.environment.site_dirs)? != session.environment.installed {
        anyhow::bail!("distributions were installed or removed during the session");
    }
    let windows = complete(&mut state, session.snapshot, &mut runs)?;
    let (stored, window_rows, unstable, changed) = rows(&state, &windows, &runs);
    let row = SessionRow {
        started: session.started,
        finished: now(),
        root: state.places.root().to_string_lossy().into_owned(),
        env_digest: session.environment.digest(),
        env_data: session.environment.value(),
        argv: session.argv.to_owned(),
        unstable,
    };
    store::write(session.store, &row, &window_rows, &stored)?;
    let mut summary = format!(
        "{} test runs recorded in {}",
        stored.len(),
        session.store.display()
    );
    if unstable > 0 {
        let named: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
        let more = changed.len().saturating_sub(named.len());
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        summary.push_str(&format!(
            "; {unstable} not recorded because a file they depend on changed during the session ({}{more})",
            named.join(", ")
        ));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_record_paths::Places;

    #[test]
    fn relative_imports_resolve_against_the_importing_package() {
        assert_eq!(absolute("os.path", None).as_deref(), Some("os.path"));
        assert_eq!(absolute(".", Some("pkg.sub")).as_deref(), Some("pkg.sub"));
        assert_eq!(
            absolute(".mod", Some("pkg.sub")).as_deref(),
            Some("pkg.sub.mod")
        );
        assert_eq!(
            absolute("..mod", Some("pkg.sub")).as_deref(),
            Some("pkg.mod")
        );
        assert_eq!(absolute("...mod", Some("pkg.sub")).as_deref(), Some("mod"));
        assert_eq!(absolute("....mod", Some("pkg.sub")), None);
        assert_eq!(absolute(".mod", None), None);
        let mut names = BTreeSet::new();
        with_parents("a.b.c", &mut names);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "a.b", "a.b.c"]);
    }

    /// A venv inside `root` where `<name>-1.0.dist-info` owns each of its files.
    fn site(root: &Path, dists: &[(&str, &[&str], &str)]) -> PathBuf {
        let site = root.join(".venv/lib/python3.12/site-packages");
        for (name, files, metadata) in dists {
            let meta = site.join(format!("{name}-1.0.dist-info"));
            std::fs::create_dir_all(&meta).unwrap();
            let record: String = files.iter().map(|file| format!("{file},,\n")).collect();
            std::fs::write(meta.join("RECORD"), record).unwrap();
            std::fs::write(meta.join("METADATA"), metadata).unwrap();
            for file in *files {
                std::fs::create_dir_all(site.join(file).parent().unwrap()).unwrap();
                std::fs::write(site.join(file), "").unwrap();
            }
        }
        site
    }

    #[test]
    fn the_recorder_is_no_dependency_and_a_held_extension_stays_with_its_window() {
        let root = std::env::temp_dir().join(format!("test-record-finish-{}", std::process::id()));
        let native: &[&str] = &["conductor_native/__init__.py", "conductor_native/native.so"];
        let site = site(
            &root,
            &[
                ("conductor_native", native, "Name: conductor-native\n"),
                (
                    "numpy",
                    &["numpy/__init__.py", "numpy/core.so"],
                    "Name: numpy\n",
                ),
                ("lonely", &["lonely/ext.so"], "Name: lonely\n"),
                (
                    "held",
                    &["held/__init__.py", "held/ext.so"],
                    "Name: held\nRequires-Dist: numpy\n\
                     Requires-Dist: conductor-native; extra == 'native'\n",
                ),
            ],
        );
        let file = |rel: &str| site.join(rel).to_string_lossy().into_owned();
        let sites = [site.to_string_lossy().into_owned()];
        let venv = root.join(".venv").to_string_lossy().into_owned();
        let places = Places::new(root.to_str().unwrap(), &[venv], &[], None)
            .with_installed(Installed::new(&sites).unwrap());
        let mut state = State::new(places);
        state.exclude(&native.iter().map(|rel| file(rel)).collect::<Vec<_>>());
        state.preloaded(&file("conductor_native/__init__.py"));
        state.push(Kind::Test, "t.py::uses_held".to_owned());
        state.hit(&file("held/__init__.py"));
        state.hit(&file("conductor_native/__init__.py"));
        state.end_test("t.py::uses_held", &root.join("t.py"), None, Vec::new(), 0.1);
        let loaded = [
            ("conductor_native.native", "conductor_native/native.so"),
            ("numpy.core", "numpy/core.so"),
            ("lonely.ext", "lonely/ext.so"),
            ("held.ext", "held/ext.so"),
        ];
        let snapshot = Snapshot {
            modules: loaded
                .iter()
                .map(|(name, rel)| ((*name).to_owned(), file(rel)))
                .collect(),
            inipath: None,
        };
        let mut runs = std::mem::take(&mut state.runs);
        let windows = complete(&mut state, &snapshot, &mut runs).unwrap();
        let labels = |window: &Window| -> Vec<String> {
            let ids = window.all_deps();
            ids.iter()
                .map(|id| state.deps[*id as usize].place.label())
                .collect()
        };
        // The recorder's distribution is in no window. The extension of lonely, which
        // no window names, is everyone's. Those of held and numpy stay with the test
        // that ran held, which requires numpy but not its optional native extra.
        assert_eq!(labels(&windows.session), ["lonely-1.0.dist-info"]);
        let mut test = labels(&runs[0].window);
        test.sort();
        assert_eq!(
            test,
            ["conftest.py", "held-1.0.dist-info", "numpy-1.0.dist-info"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
