//! Session end: the static import closure, the environment key, the stability
//! check, and the rows the store writes.

use crate::graph_index;
use crate::test_record_digest::{self as digest, sha1_hex};
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

/// The environment key: a record is reusable only under an equal key.
pub(crate) struct Environment {
    pub(crate) python: String,
    pub(crate) vars: BTreeMap<String, Option<String>>,
    pub(crate) site_dirs: Vec<String>,
    pub(crate) dists: BTreeMap<String, String>,
}

impl Environment {
    fn value(&self) -> Value {
        json!({"python": self.python, "dists": self.dists, "vars": self.vars})
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

/// Session-wide dependencies pytest and the interpreter load outside any window.
fn session_extras(state: &mut State, snapshot: &Snapshot, session: &mut Window) {
    let extensions = snapshot
        .modules
        .iter()
        .map(|(_, file)| file.as_str())
        .filter(|file| file.ends_with(".so") || file.ends_with(".pyd"));
    for file in extensions.chain(snapshot.inipath.as_deref()) {
        if let Some((place, real)) = state.locate(Path::new(file)) {
            session.deps.insert(state.dep(DepKind::File, place, real));
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
    session_extras(state, snapshot, &mut session);
    closure.extend(state, &modules, &mut session)?;
    for window in shared.values_mut() {
        closure.extend(state, &modules, window)?;
    }
    for run in runs {
        closure.extend(state, &modules, &mut run.window)?;
    }
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
    if digest::distributions(&session.environment.site_dirs)? != session.environment.dists {
        anyhow::bail!("installed distributions changed during the session");
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
}
