//! Recorder bookkeeping: the dependency table and the window stack. Pure Rust, so
//! attribution is testable without an interpreter.

use crate::test_record_digest::{self as digest, Stat, ABSENT};
use crate::test_record_paths::{Place, Places};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where a hit is attributed. A test depends on the session window, the collect
/// window of its module, the windows of the fixtures it used, and its own.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Kind {
    Session,
    Collect,
    Fixture,
    Test,
}

impl Kind {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "collect" => Some(Self::Collect),
            "fixture" => Some(Self::Fixture),
            "test" => Some(Self::Test),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Collect => "collect",
            Self::Fixture => "fixture",
            Self::Test => "test",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum DepKind {
    File,
    Listing,
    /// An installed distribution, by the digest of the files it installs.
    Installed,
}

impl DepKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Listing => "list",
            Self::Installed => "inst",
        }
    }
}

/// What an audited event means for the current window.
pub(crate) enum Access {
    Read(PathBuf),
    List(PathBuf),
    /// A modeled program ran: its output is a function of these files.
    Program(&'static [&'static str]),
    Opaque(&'static str),
}

pub(crate) struct Dep {
    pub(crate) kind: DepKind,
    pub(crate) place: Place,
    pub(crate) real: PathBuf,
    /// The value at first sight; a different value at session end makes it unstable.
    pub(crate) first: String,
}

impl Dep {
    pub(crate) fn current(&self) -> String {
        match (self.kind, &self.place) {
            (DepKind::File, Place::Repo(_)) => std::fs::read(&self.real)
                .map(|bytes| digest::blob_oid(&bytes))
                .unwrap_or_else(|_| ABSENT.to_owned()),
            (DepKind::File, _) => {
                Stat::read(&self.real).map_or_else(|| ABSENT.to_owned(), |stat| stat.key())
            }
            (DepKind::Listing, _) => {
                digest::listing(&self.real).unwrap_or_else(|_| ABSENT.to_owned())
            }
            (DepKind::Installed, _) => digest::distribution(&self.real),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Window {
    /// Files and listings read, and executed code outside the repository.
    pub(crate) deps: BTreeSet<u32>,
    /// Executed repository files and their hit lines, as bitsets.
    pub(crate) executed: BTreeMap<u32, Vec<u64>>,
    pub(crate) opaque: Option<String>,
}

impl Window {
    /// An executed file is often also an import dependency; it is listed once.
    pub(crate) fn all_deps(&self) -> BTreeSet<u32> {
        let mut all = self.deps.clone();
        all.extend(self.executed.keys());
        all
    }
}

fn set_bit(bits: &mut Vec<u64>, line: usize) {
    let (word, bit) = (line / 64, line % 64);
    if bits.len() <= word {
        bits.resize(word + 1, 0);
    }
    bits[word] |= 1 << bit;
}

pub(crate) fn bit_lines(bits: &[u64]) -> Vec<usize> {
    let mut lines = Vec::new();
    for (word, value) in bits.iter().enumerate() {
        for bit in 0..64 {
            if value & (1 << bit) != 0 {
                lines.push(word * 64 + bit);
            }
        }
    }
    lines
}

pub(crate) struct Run {
    pub(crate) nodeid: String,
    pub(crate) path: String,
    pub(crate) outcome: &'static str,
    pub(crate) seconds: f64,
    pub(crate) module: Option<String>,
    pub(crate) fixtures: Vec<String>,
    pub(crate) window: Window,
}

/// A test's outcome over its phases: the worst phase wins. An outcome a plugin
/// invented (a `rerun`, say) is never a pass.
fn phase_outcome(when: &str, outcome: &str, wasxfail: bool) -> (u8, &'static str) {
    match (outcome, wasxfail) {
        ("failed", _) if when == "call" => (3, "failed"),
        ("failed", _) => (3, "error"),
        ("skipped", true) => (2, "xfailed"),
        ("skipped", false) => (2, "skipped"),
        ("passed", true) => (1, "xpassed"),
        ("passed", false) => (0, "passed"),
        _ => (3, "other"),
    }
}

pub(crate) struct State {
    pub(crate) places: Places,
    pub(crate) deps: Vec<Dep>,
    dep_ids: HashMap<(DepKind, Place), u32>,
    /// `co_filename` to (dependency, is repository source).
    code_files: HashMap<String, Option<(u32, bool)>>,
    /// The recorder's own files: they run in every window, and depending on the
    /// distribution that ships them would rerun every test whenever it is rebuilt.
    instrument: HashSet<PathBuf>,
    pub(crate) stack: Vec<(Kind, String, Window)>,
    /// Collect and fixture windows accumulate over every setup with the same key.
    pub(crate) closed: HashMap<(Kind, String), Window>,
    pub(crate) runs: Vec<Run>,
    outcomes: HashMap<String, (u8, &'static str)>,
    pub(crate) failure: Option<String>,
}

impl State {
    pub(crate) fn new(places: Places) -> Self {
        Self {
            places,
            deps: Vec::new(),
            dep_ids: HashMap::new(),
            code_files: HashMap::new(),
            instrument: HashSet::new(),
            stack: vec![(Kind::Session, String::new(), Window::default())],
            closed: HashMap::new(),
            runs: Vec::new(),
            outcomes: HashMap::new(),
            failure: None,
        }
    }

    pub(crate) fn fail(&mut self, reason: String) {
        self.failure.get_or_insert(reason);
    }

    fn current(&mut self) -> &mut Window {
        &mut self
            .stack
            .last_mut()
            .expect("the session window is never popped")
            .2
    }

    pub(crate) fn locate(&mut self, raw: &Path) -> Option<(Place, PathBuf)> {
        if raw.is_absolute() {
            return self.places.classify(raw, Path::new("/"));
        }
        match std::env::current_dir() {
            Ok(cwd) => self.places.classify(raw, &cwd),
            Err(error) => {
                self.fail(format!("cannot resolve {}: {error}", raw.display()));
                None
            }
        }
    }

    /// Excludes the recorder's own files, given as `__file__` paths, from every window.
    pub(crate) fn exclude(&mut self, files: &[String]) {
        for file in files {
            if let Some((_, real)) = self.locate(Path::new(file)) {
                self.instrument.insert(real);
            }
        }
    }

    /// The dependency `raw` names, and whether it is repository source; `None` when
    /// it is no dependency or is the recorder's own file.
    pub(crate) fn located(&mut self, kind: DepKind, raw: &Path) -> Option<(u32, bool)> {
        let (place, real) = self.locate(raw)?;
        if self.instrument.contains(&real) {
            return None;
        }
        let source = matches!(place, Place::Repo(_));
        Some((self.dep(kind, place, real), source))
    }

    /// The id of a dependency already seen, without adding it.
    pub(crate) fn known(&self, kind: DepKind, place: &Place) -> Option<u32> {
        self.dep_ids.get(&(kind, place.clone())).copied()
    }

    /// A file a distribution owns is that distribution, read from its metadata.
    pub(crate) fn dep(&mut self, kind: DepKind, place: Place, real: PathBuf) -> u32 {
        let (kind, real) = match &place {
            Place::Dist(label) => (DepKind::Installed, self.places.dist(label).1),
            _ => (kind, real),
        };
        if let Some(id) = self.dep_ids.get(&(kind, place.clone())) {
            return *id;
        }
        let id = u32::try_from(self.deps.len()).expect("fewer than 2^32 dependencies");
        let mut dep = Dep {
            kind,
            place: place.clone(),
            real,
            first: String::new(),
        };
        dep.first = dep.current();
        self.deps.push(dep);
        self.dep_ids.insert((kind, place), id);
        id
    }

    fn code_file(&mut self, filename: &str) -> Option<(u32, bool)> {
        if let Some(known) = self.code_files.get(filename) {
            return *known;
        }
        let entry = if filename.starts_with('<') {
            None
        } else {
            self.located(DepKind::File, Path::new(filename))
        };
        self.code_files.insert(filename.to_owned(), entry);
        entry
    }

    /// A code object started. True when it is repository source, which gets line events.
    pub(crate) fn hit(&mut self, filename: &str) -> bool {
        match self.code_file(filename) {
            Some((id, true)) => {
                self.current().executed.entry(id).or_default();
                true
            }
            Some((id, false)) => {
                self.current().deps.insert(id);
                false
            }
            None => false,
        }
    }

    pub(crate) fn line(&mut self, filename: &str, line: usize) {
        if let Some((id, true)) = self.code_file(filename) {
            set_bit(self.current().executed.entry(id).or_default(), line);
        }
    }

    pub(crate) fn access(&mut self, access: Access) {
        match access {
            Access::Read(raw) => self.read(DepKind::File, &raw),
            Access::List(raw) => self.read(DepKind::Listing, &raw),
            Access::Program(files) => {
                for file in files {
                    self.read(DepKind::File, Path::new(file));
                }
            }
            Access::Opaque(reason) => {
                self.current()
                    .opaque
                    .get_or_insert_with(|| reason.to_owned());
            }
        }
    }

    fn read(&mut self, kind: DepKind, raw: &Path) {
        if let Some((id, _)) = self.located(kind, raw) {
            self.current().deps.insert(id);
        }
    }

    /// A module imported before recording started is a dependency of every test.
    pub(crate) fn preloaded(&mut self, file: &str) {
        if let Some((id, _)) = self.located(DepKind::File, Path::new(file)) {
            self.stack[0].2.deps.insert(id);
        }
    }

    pub(crate) fn push(&mut self, kind: Kind, key: String) {
        let window = match kind {
            Kind::Collect | Kind::Fixture => {
                self.closed.remove(&(kind, key.clone())).unwrap_or_default()
            }
            Kind::Session | Kind::Test => Window::default(),
        };
        self.stack.push((kind, key, window));
    }

    fn pop_window(&mut self, kind: Kind, key: &str) -> Result<Window, String> {
        match self.stack.last() {
            Some((top, top_key, _)) if self.stack.len() > 1 && *top == kind && top_key == key => {
                Ok(self.stack.pop().expect("checked non-empty").2)
            }
            Some((top, top_key, _)) => Err(format!(
                "window {} {key:?} closed while {} {top_key:?} is innermost",
                kind.name(),
                top.name()
            )),
            None => Err("window stack is empty".to_owned()),
        }
    }

    pub(crate) fn pop(&mut self, kind: Kind, key: &str) {
        match self.pop_window(kind, key) {
            Ok(window) => {
                self.closed.insert((kind, key.to_owned()), window);
            }
            Err(error) => self.fail(error),
        }
    }

    pub(crate) fn report(&mut self, nodeid: &str, when: &str, outcome: &str, wasxfail: bool) {
        let phase = phase_outcome(when, outcome, wasxfail);
        let entry = self.outcomes.entry(nodeid.to_owned()).or_insert(phase);
        if phase.0 > entry.0 {
            *entry = phase;
        }
    }

    /// `path` is the test file; the node id is re-rooted on it so records from
    /// different checkouts and pytest rootdirs of one repository agree.
    pub(crate) fn end_test(
        &mut self,
        nodeid: &str,
        path: &Path,
        module: Option<String>,
        fixtures: Vec<String>,
        seconds: f64,
    ) {
        let mut window = match self.pop_window(Kind::Test, nodeid) {
            Ok(window) => window,
            Err(error) => return self.fail(error),
        };
        let (canonical, rel) = self.places.rerooted(nodeid, path);
        // A conftest added later in any ancestor directory changes the test.
        for dir in Some(rel.as_path())
            .filter(|rel| rel.is_relative())
            .and_then(Path::parent)
            .into_iter()
            .flat_map(Path::ancestors)
        {
            let conftest = self.places.root().join(dir).join("conftest.py");
            if let Some((place, real)) = self.locate(&conftest) {
                window.deps.insert(self.dep(DepKind::File, place, real));
            }
        }
        let rel = rel.to_string_lossy().into_owned();
        let outcome = self
            .outcomes
            .remove(nodeid)
            .map_or("missing", |(_, name)| name);
        self.runs.push(Run {
            nodeid: canonical,
            path: rel,
            outcome,
            seconds,
            module,
            fixtures,
            window,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(root: &Path) -> State {
        State::new(Places::new(root.to_str().unwrap(), &[], &[], None))
    }

    #[test]
    fn hits_go_to_the_innermost_window_and_fixture_windows_accumulate() {
        let root = std::env::temp_dir().join(format!("test-record-state-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("lib.py"), "x = 1\n").unwrap();
        std::fs::write(root.join("tests/test_a.py"), "def test(): pass\n").unwrap();
        let lib = root.join("lib.py").to_string_lossy().into_owned();
        let mut state = state(&root);
        state.push(Kind::Fixture, "tests::f".to_owned());
        assert!(state.hit(&lib));
        state.line(&lib, 70);
        state.pop(Kind::Fixture, "tests::f");
        state.push(Kind::Test, "tests/test_a.py::test".to_owned());
        state.push(Kind::Fixture, "tests::f".to_owned());
        state.line(&lib, 3);
        state.pop(Kind::Fixture, "tests::f");
        state.access(Access::Opaque("subprocess.Popen"));
        state.report("tests/test_a.py::test", "setup", "passed", false);
        state.report("tests/test_a.py::test", "call", "failed", false);
        state.report("tests/test_a.py::test", "teardown", "passed", false);
        state.end_test(
            "tests/test_a.py::test",
            &root.join("tests/test_a.py"),
            Some("tests/test_a.py".to_owned()),
            vec!["tests::f".to_owned()],
            0.1,
        );
        assert!(state.failure.is_none());
        let fixture = &state.closed[&(Kind::Fixture, "tests::f".to_owned())];
        let lines: Vec<usize> = fixture
            .executed
            .values()
            .flat_map(|bits| bit_lines(bits))
            .collect();
        assert_eq!(lines, [3, 70]);
        let run = &state.runs[0];
        assert_eq!(
            (run.outcome, run.path.as_str()),
            ("failed", "tests/test_a.py")
        );
        assert_eq!(run.window.opaque.as_deref(), Some("subprocess.Popen"));
        assert!(run.window.executed.is_empty());
        let conftests: Vec<String> = run
            .window
            .deps
            .iter()
            .map(|id| state.deps[*id as usize].place.label())
            .collect();
        assert_eq!(conftests, ["tests/conftest.py", "conftest.py"]);
        assert!(state
            .deps
            .iter()
            .all(|dep| dep.place.label() != "lib.py" || dep.first == digest::blob_oid(b"x = 1\n")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_modeled_program_is_a_read_of_its_files_and_not_opaque() {
        let mut state = state(Path::new("/nonexistent-record-root"));
        state.push(Kind::Test, "tests/test_a.py::test".to_owned());
        state.access(Access::Program(&[
            "/nonexistent/program",
            "/nonexistent/cache",
        ]));
        let window = std::mem::take(state.current());
        assert!(window.opaque.is_none());
        let deps: Vec<(String, String)> = window
            .deps
            .into_iter()
            .map(|id| &state.deps[id as usize])
            .map(|dep| (dep.place.label(), dep.first.clone()))
            .collect();
        let absent = |path: &str| (path.to_owned(), ABSENT.to_owned());
        assert_eq!(
            deps,
            [absent("/nonexistent/program"), absent("/nonexistent/cache")]
        );
    }

    #[test]
    fn a_mismatched_pop_fails_the_session_instead_of_misattributing() {
        let mut state = state(&std::env::temp_dir());
        state.push(Kind::Collect, "a.py".to_owned());
        state.pop(Kind::Fixture, "x");
        assert!(state.failure.unwrap().contains("innermost"));
    }
}
