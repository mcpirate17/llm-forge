//! Rust fixture ownership for probing Python source as test input.
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MODULE: &str = include_str!("../fixtures/equivalence_probe/fixture_mod.py");
const TESTS: &str = include_str!("../fixtures/equivalence_probe/test_fixture_mod.py");
const LOOPING_TESTS: &str =
    include_str!("../fixtures/equivalence_probe/test_fixture_mod_looping.py");
const METHOD_MODULE: &str = include_str!("../fixtures/equivalence_probe/method_fixture_mod.py");
const METHOD_TESTS: &str = include_str!("../fixtures/equivalence_probe/test_method_fixture_mod.py");
const UNARY_MODULE: &str = include_str!("../fixtures/equivalence_probe/unary_fixture.py");
const REPLAY_SUBJECTS: &str = include_str!("../fixtures/equivalence_probe/replay_subjects.py");

pub struct ProbeWorkspace {
    pub _cwd: crate::support::CwdRestore,
    pub _case: Case,
    pub module_path: PathBuf,
}
impl ProbeWorkspace {
    pub fn new() -> Self {
        Self::with_sources(MODULE, TESTS)
    }
    pub fn looping() -> Self {
        Self::with_sources(MODULE, LOOPING_TESTS)
    }
    pub fn methods() -> Self {
        Self::with_sources(METHOD_MODULE, METHOD_TESTS)
    }
    pub fn with_sources(source: &str, tests: &str) -> Self {
        let case = Case::new();
        case.write("pytest.ini", "[pytest]\n");
        let module_path = case.write("fixture_mod.py", source);
        case.write("test_fixture_mod.py", tests);
        let cwd = case.chdir(".");
        Python::attach(|py| {
            let sys = module(py, "sys");
            sys.getattr("path")
                .unwrap()
                .call_method1("insert", (0, case.root().to_str().unwrap()))
                .unwrap();
            module(py, "importlib")
                .call_method0("invalidate_caches")
                .unwrap();
            for name in ["fixture_mod", "test_fixture_mod"] {
                sys.getattr("modules")
                    .unwrap()
                    .call_method1("pop", (name, py.None()))
                    .unwrap();
            }
        });
        Self {
            _cwd: cwd,
            _case: case,
            module_path,
        }
    }
    pub fn module_path<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        path(py, &self.module_path)
    }
}

pub fn ep(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.equivalence_probe")
}
pub fn abl(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.equivalence_ablations")
}
pub fn prs(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.probe_replay_stubs")
}

pub fn probe<'py>(py: Python<'py>, ws: &ProbeWorkspace, qualname: &str) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("module_name", "fixture_mod").unwrap();
    ep(py)
        .getattr("probe_function")
        .unwrap()
        .call(
            (
                ws.module_path(py),
                qualname,
                PyList::new(py, ["test_fixture_mod.py"]).unwrap(),
            ),
            Some(&kwargs),
        )
        .unwrap()
}
pub fn verdicts(py: Python<'_>, ws: &ProbeWorkspace, qualname: &str) -> Vec<(String, String)> {
    probe(py, ws, qualname)
        .try_iter()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (
                r.getattr("rule").unwrap().extract().unwrap(),
                r.getattr("verdict").unwrap().extract().unwrap(),
            )
        })
        .collect()
}
pub fn verdict<'py>(py: Python<'py>, ws: &ProbeWorkspace, qualname: &str, rule: &str) -> String {
    verdicts(py, ws, qualname)
        .into_iter()
        .find(|(r, _)| r == rule)
        .unwrap_or_else(|| panic!("missing {rule}"))
        .1
}
pub fn verdict_constant(py: Python<'_>, name: &str) -> String {
    ep(py)
        .getattr("Verdict")
        .unwrap()
        .getattr(name)
        .unwrap()
        .extract()
        .unwrap()
}
pub fn results(py: Python<'_>, ws: &ProbeWorkspace, qualname: &str) -> Vec<Py<PyAny>> {
    probe(py, ws, qualname)
        .try_iter()
        .unwrap()
        .map(|r| r.unwrap().unbind())
        .collect()
}
pub fn result_verdict(result: &Bound<'_, PyAny>) -> String {
    result.getattr("verdict").unwrap().extract().unwrap()
}
pub fn rules(py: Python<'_>, ws: &ProbeWorkspace, qualname: &str) -> Vec<String> {
    verdicts(py, ws, qualname)
        .into_iter()
        .map(|(r, _)| r)
        .collect()
}
pub fn fixture_ast<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "ast")
        .getattr("parse")
        .unwrap()
        .call1((MODULE,))
        .unwrap()
}
pub fn unary_ast<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "ast")
        .getattr("parse")
        .unwrap()
        .call1((UNARY_MODULE,))
        .unwrap()
}
pub fn named_child<'py>(nodes: &Bound<'py, PyAny>, name: &str) -> Bound<'py, PyAny> {
    nodes
        .try_iter()
        .unwrap()
        .find_map(|item| {
            let item = item.unwrap();
            (item
                .getattr("name")
                .ok()
                .and_then(|n| n.extract::<String>().ok())
                .as_deref()
                == Some(name))
            .then_some(item)
        })
        .unwrap_or_else(|| panic!("missing AST node {name}"))
}
pub fn generated_rules(
    py: Python<'_>,
    node: &Bound<'_, PyAny>,
    tree: Option<&Bound<'_, PyAny>>,
) -> Vec<String> {
    let out = match tree {
        Some(tree) => abl(py)
            .getattr("generate_ablations")
            .unwrap()
            .call1((node, tree))
            .unwrap(),
        None => abl(py)
            .getattr("generate_ablations")
            .unwrap()
            .call1((node,))
            .unwrap(),
    };
    out.try_iter()
        .unwrap()
        .map(|a| a.unwrap().getattr("rule").unwrap().extract().unwrap())
        .collect()
}
pub fn import_fixture(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "fixture_mod")
}
pub fn public_targets(py: Python<'_>, ws: &ProbeWorkspace) -> Vec<String> {
    ep(py)
        .getattr("public_functions")
        .unwrap()
        .call1((ws.module_path(py),))
        .unwrap()
        .extract()
        .unwrap()
}

#[pyclass(name = "_Uncopyable", module = "replay_subjects")]
struct Uncopyable {
    #[pyo3(get)]
    items: Py<PyList>,
    lock: Py<PyAny>,
}

#[pymethods]
impl Uncopyable {
    #[new]
    fn new(py: Python<'_>, items: Vec<String>) -> Self {
        Self {
            items: PyList::new(py, items).unwrap().unbind(),
            lock: module(py, "threading")
                .getattr("Lock")
                .unwrap()
                .call0()
                .unwrap()
                .unbind(),
        }
    }

    fn __deepcopy__(&self, py: Python<'_>, _memo: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        // The real-world failure mode is an object carrying a live lock.
        module(py, "copy")
            .getattr("deepcopy")?
            .call1((self.lock.bind(py),))
            .map(Bound::unbind)
    }
}

#[pyfunction(name = "_consume")]
fn consume(payload: &Bound<'_, PyAny>) -> PyResult<usize> {
    let items = payload.getattr("items")?;
    if items.len()? == 0 {
        return Ok(0);
    }
    let value: String = items.call_method0("pop")?.extract()?;
    Ok(value.len())
}

#[pyfunction(name = "_real_embed")]
fn real_embed(_text: &str) -> PyResult<Vec<f64>> {
    Err(pyo3::exceptions::PyAssertionError::new_err(
        "live broker reached",
    ))
}

#[pyfunction(name = "_stub_embed")]
fn stub_embed(_text: &str) -> Vec<f64> {
    vec![0.0]
}

pub fn clone_subject_module(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    let subjects = pyo3::types::PyModule::from_code(
        py,
        &std::ffi::CString::new(REPLAY_SUBJECTS).unwrap(),
        c"replay_subjects.py",
        c"replay_subjects",
    )
    .unwrap();
    subjects.add_class::<Uncopyable>().unwrap();
    subjects
        .add_function(wrap_pyfunction!(consume, &subjects).unwrap())
        .unwrap();
    subjects
        .add_function(wrap_pyfunction!(real_embed, &subjects).unwrap())
        .unwrap();
    subjects
        .add_function(wrap_pyfunction!(stub_embed, &subjects).unwrap())
        .unwrap();
    let real_path = path(py, Path::new("/server/real/index.jsonl"));
    subjects.add("real_path", &real_path).unwrap();
    let real = subjects.getattr("_real_embed").unwrap();
    subjects
        .getattr("query")
        .unwrap()
        .setattr("__defaults__", (&real, &real_path))
        .unwrap();
    subjects
}

/// Wrap pytest.main to count driver invocations while delegating to the actual runner.
pub fn count_pytest_runs(py: Python<'_>) -> (AttrPatch, Arc<Mutex<usize>>) {
    let pytest = module(py, "pytest");
    let real = pytest.getattr("main").unwrap().unbind();
    let count = Arc::new(Mutex::new(0));
    let captured = count.clone();
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        *captured.lock().unwrap() += 1;
        real.bind(args.py())
            .call(args, kwargs)
            .map(|out| out.unbind())
    })
    .unwrap();
    (
        AttrPatch::replace(pytest.as_any(), "main", callback.as_any()),
        count,
    )
}

pub fn create_stub_target<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyAny> {
    module(py, "types")
        .getattr("ModuleType")
        .unwrap()
        .call1((name,))
        .unwrap()
}

/// Restore the Python context and test registry even when a Rust assertion unwinds.
pub struct ReplayStubOverride {
    context: Py<PyAny>,
    stubs: Py<PyAny>,
    key: String,
    active: bool,
}

impl ReplayStubOverride {
    pub fn new(py: Python<'_>, target: &Bound<'_, PyAny>, spec: &Bound<'_, PyDict>) -> Self {
        let key = target
            .getattr("__name__")
            .unwrap()
            .extract::<String>()
            .unwrap();
        let stubs = prs(py).getattr("REPLAY_STUBS").unwrap();
        stubs.set_item(&key, spec).unwrap();
        let context = prs(py)
            .getattr("replay_stub_overrides")
            .unwrap()
            .call1((target,))
            .unwrap();
        if let Err(error) = context.call_method0("__enter__") {
            stubs.del_item(&key).unwrap();
            panic!("enter replay stub override: {error}");
        }
        Self {
            context: context.unbind(),
            stubs: stubs.unbind(),
            key,
            active: true,
        }
    }

    pub fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        if !self.active {
            return Ok(());
        }
        let exit = self
            .context
            .bind(py)
            .call_method1("__exit__", (py.None(), py.None(), py.None()));
        let delete = self.stubs.bind(py).del_item(self.key.as_str());
        self.active = false;
        exit?;
        delete
    }
}

impl Drop for ReplayStubOverride {
    fn drop(&mut self) {
        if self.active {
            Python::attach(|py| {
                if let Err(error) = self.close(py) {
                    if !std::thread::panicking() {
                        panic!("restore replay stub override: {error}");
                    }
                }
            });
        }
    }
}
