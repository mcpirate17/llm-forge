//! Explicit Python/PyTorch guest for the native equivalence runtime.
//! All state, recording, transformation and adjudication are Rust-owned.
use crate::equivalence_runtime::{self, Guest};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

type Call = (Py<PyAny>, Py<PyAny>);
#[derive(Default)]
struct Recording {
    calls: Vec<Call>,
    dropped: Vec<String>,
}

fn torch<'py>(py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
    py.import("sys")?
        .getattr("modules")?
        .cast::<PyDict>()?
        .get_item("torch")
}
fn is_tensor(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    match torch(value.py())? {
        Some(torch) => torch.call_method1("is_tensor", (value,))?.is_truthy(),
        None => Ok(false),
    }
}
fn clone_value<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = value.py();
    if is_tensor(value)? {
        return value.call_method0("detach")?.call_method0("clone");
    }
    if value.is_instance_of::<PyList>() || value.is_instance_of::<PyTuple>() {
        let values = PyList::empty(py);
        for item in value.try_iter()? {
            values.append(clone_value(&item?)?)?;
        }
        return value.get_type().call1((values,));
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let out = PyDict::new(py);
        for (key, item) in dict.iter() {
            out.set_item(key, clone_value(&item)?)?;
        }
        return Ok(out.into_any());
    }
    py.import("copy")?.call_method1("deepcopy", (value,))
}
#[pyclass(unsendable, dict)]
struct Recorder {
    target: Py<PyAny>,
    recording: Rc<RefCell<Recording>>,
}
#[pymethods]
impl Recorder {
    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        if self.recording.borrow().calls.len() + self.recording.borrow().dropped.len() < 24 {
            let kwargs_owned = kwargs.cloned().unwrap_or_else(|| PyDict::new(py));
            match (
                clone_value(args.as_any()),
                clone_value(kwargs_owned.as_any()),
            ) {
                (Ok(a), Ok(k)) => self
                    .recording
                    .borrow_mut()
                    .calls
                    .push((a.unbind(), k.unbind())),
                _ => self
                    .recording
                    .borrow_mut()
                    .dropped
                    .push("uncopyable".into()),
            }
        }
        Ok(self.target.bind(py).call(args, kwargs)?.unbind())
    }
    #[getter]
    fn __equivalence_recorder__(&self) -> bool {
        true
    }
    fn __get__<'py>(
        slf: PyRef<'py, Self>,
        py: Python<'py>,
        object: &Bound<'py, PyAny>,
        _owner: &Bound<'py, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        if object.is_none() {
            return Ok(slf.into_pyobject(py)?.into_any().unbind());
        }
        Ok(py
            .import("types")?
            .getattr("MethodType")?
            .call1((slf.into_pyobject(py)?, object))?
            .unbind())
    }
}

fn compile<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    source: &str,
    qualname: &str,
    decorated: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let ast = py.import("ast")?;
    let mut node = ast.call_method1("parse", (source,))?;
    for name in qualname.split('.') {
        let mut found = None;
        for child in ast.call_method1("iter_child_nodes", (&node,))?.try_iter()? {
            let child = child?;
            let kind = child.get_type().name()?.to_string();
            if ["FunctionDef", "AsyncFunctionDef", "ClassDef"].contains(&kind.as_str())
                && child.getattr("name")?.extract::<String>()? == name
            {
                found = Some(child);
                break;
            }
        }
        node = found.ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("{qualname} not found"))
        })?;
    }
    let kind = node.get_type().name()?.to_string();
    if !["FunctionDef", "AsyncFunctionDef"].contains(&kind.as_str()) {
        return Err(pyo3::exceptions::PyLookupError::new_err(format!(
            "{qualname} is not a function"
        )));
    }
    let name = node.getattr("name")?;
    if !decorated {
        node.setattr("decorator_list", PyList::empty(py))?;
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("body", PyList::new(py, [&node])?)?;
    kwargs.set_item("type_ignores", PyList::empty(py))?;
    let holder = ast.getattr("Module")?.call((), Some(&kwargs))?;
    ast.call_method1("fix_missing_locations", (&holder,))?;
    let builtins = py.import("builtins")?;
    let filename = module.getattr("__file__")?;
    let code = builtins.call_method1("compile", (&holder, filename, "exec"))?;
    let namespace = PyDict::new(py);
    builtins.call_method1("exec", (code, module.getattr("__dict__")?, &namespace))?;
    namespace
        .get_item(name)?
        .ok_or_else(|| pyo3::exceptions::PyLookupError::new_err("compiled function missing"))
}

fn difference<'py>(a: &Bound<'py, PyAny>, b: &Bound<'py, PyAny>) -> PyResult<f64> {
    let py = a.py();
    if is_tensor(a)? && is_tensor(b)? {
        if !a.getattr("shape")?.eq(b.getattr("shape")?)?
            || !a.getattr("dtype")?.eq(b.getattr("dtype")?)?
        {
            return Ok(f64::INFINITY);
        }
        if a.call_method0("is_floating_point")?.is_truthy()? {
            let x = a.call_method0("detach")?.call_method0("double")?;
            let y = b.call_method0("detach")?.call_method0("double")?;
            let delta = x.call_method1("__sub__", (y,))?.call_method0("abs")?;
            return if delta.call_method0("numel")?.extract::<usize>()? == 0 {
                Ok(0.0)
            } else {
                delta.call_method0("max")?.extract()
            };
        }
        return Ok(
            if torch(py)?
                .unwrap()
                .call_method1("equal", (a, b))?
                .is_truthy()?
            {
                0.0
            } else {
                f64::INFINITY
            },
        );
    }
    let sequence =
        |x: &Bound<'_, PyAny>| x.is_instance_of::<PyList>() || x.is_instance_of::<PyTuple>();
    if sequence(a) && sequence(b) {
        if a.len()? != b.len()? {
            return Ok(f64::INFINITY);
        }
        let mut max = 0.0_f64;
        for (x, y) in a.try_iter()?.zip(b.try_iter()?) {
            let d = difference(&x?, &y?)?;
            if d > max {
                max = d;
            }
        }
        return Ok(max);
    }
    if let (Ok(a), Ok(b)) = (a.cast::<PyDict>(), b.cast::<PyDict>()) {
        if !a.call_method0("keys")?.eq(b.call_method0("keys")?)? {
            return Ok(f64::INFINITY);
        }
        let mut max = 0.0_f64;
        for (key, item) in a.iter() {
            let d = difference(&item, &b.get_item(key)?.unwrap())?;
            if d > max {
                max = d;
            }
        }
        return Ok(max);
    }
    let builtins = py.import("builtins")?;
    let number = PyTuple::new(py, [builtins.getattr("int")?, builtins.getattr("float")?])?;
    if a.is_instance(number.as_any())? && b.is_instance(number.as_any())? {
        return Ok((a.extract::<f64>()? - b.extract::<f64>()?).abs());
    }
    Ok(if a.eq(b).unwrap_or(false) {
        0.0
    } else {
        f64::INFINITY
    })
}
fn magnitude(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    let py = value.py();
    if is_tensor(value)? {
        if value.call_method0("numel")?.extract::<usize>()? == 0 {
            return Ok(0.0);
        }
        return value
            .call_method0("detach")?
            .call_method0("abs")?
            .call_method0("max")?
            .extract();
    }
    let mut max = 0.0_f64;
    if value.is_instance_of::<PyList>() || value.is_instance_of::<PyTuple>() {
        for item in value.try_iter()? {
            let n = magnitude(&item?)?;
            if n > max {
                max = n;
            }
        }
        return Ok(max);
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        for (_, item) in dict.iter() {
            let n = magnitude(&item)?;
            if n > max {
                max = n;
            }
        }
        return Ok(max);
    }
    let builtins = py.import("builtins")?;
    if !value.is_instance(&builtins.getattr("bool")?)?
        && value.is_instance(
            PyTuple::new(py, [builtins.getattr("int")?, builtins.getattr("float")?])?.as_any(),
        )?
    {
        return Ok(value.extract::<f64>()?.abs());
    }
    Ok(0.0)
}
fn amplify<'py>(
    value: &Bound<'py, PyAny>,
    factor: f64,
    params: bool,
) -> PyResult<(Bound<'py, PyAny>, bool)> {
    let py = value.py();
    if params {
        if let Some(torch) = torch(py)? {
            if value.is_instance(&torch.getattr("nn")?.getattr("Module")?)? {
                let copy = py.import("copy")?.call_method1("deepcopy", (value,))?;
                let guard = torch.call_method0("no_grad")?;
                guard.call_method0("__enter__")?;
                let result: PyResult<()> = (|| {
                    for param in copy.call_method0("parameters")?.try_iter()? {
                        let param = param?;
                        if param.call_method0("is_floating_point")?.is_truthy()? {
                            param.call_method1("mul_", (factor,))?;
                        }
                    }
                    Ok(())
                })();
                guard.call_method1("__exit__", (py.None(), py.None(), py.None()))?;
                result?;
                return Ok((copy, true));
            }
        }
        return Ok((value.clone(), false));
    }
    if is_tensor(value)? && value.call_method0("is_floating_point")?.is_truthy()? {
        return Ok((value.call_method1("__mul__", (factor,))?, true));
    }
    if value.is_instance_of::<PyList>() || value.is_instance_of::<PyTuple>() {
        let result = PyList::empty(py);
        let mut changed = false;
        for item in value.try_iter()? {
            let (item, was_changed) = amplify(&item?, factor, false)?;
            result.append(item)?;
            changed |= was_changed;
        }
        return Ok((value.get_type().call1((result,))?, changed));
    }
    Ok((value.clone(), false))
}
fn transform<'py>(
    args: &Bound<'py, PyAny>,
    kwargs: &Bound<'py, PyAny>,
    factor: f64,
    params: bool,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>, bool)> {
    let py = args.py();
    let output = PyList::empty(py);
    let mut changed = false;
    for arg in args.try_iter()? {
        let (arg, altered) = amplify(&arg?, factor, params)?;
        output.append(arg)?;
        changed |= altered;
    }
    let result_args = PyTuple::new(py, output.iter())?.into_any();
    let result_kwargs = PyDict::new(py);
    for (key, value) in kwargs.cast::<PyDict>()?.iter() {
        let (value, altered) = amplify(&value, factor, params)?;
        result_kwargs.set_item(key, value)?;
        changed |= altered;
    }
    Ok((result_args, result_kwargs.into_any(), changed))
}

enum Outcome {
    Value(Py<PyAny>),
    Raised(Py<PyAny>),
}
impl Outcome {
    fn copy(&self, py: Python<'_>) -> Self {
        match self {
            Self::Value(value) => Self::Value(value.clone_ref(py)),
            Self::Raised(value) => Self::Raised(value.clone_ref(py)),
        }
    }
}
fn invoke<'py>(
    py: Python<'py>,
    function: &Bound<'py, PyAny>,
    args: &Bound<'py, PyAny>,
    kwargs: &Bound<'py, PyAny>,
) -> PyResult<Outcome> {
    match function.call(args.cast::<PyTuple>()?, Some(kwargs.cast::<PyDict>()?)) {
        Ok(value) => Ok(Outcome::Value(value.unbind())),
        Err(error)
            if error.is_instance_of::<pyo3::exceptions::PyException>(py)
                || error.is_instance_of::<pyo3::exceptions::PySystemExit>(py) =>
        {
            Ok(Outcome::Raised(error.get_type(py).into_any().unbind()))
        }
        Err(error) => Err(error),
    }
}
struct PythonGuest<'py> {
    py: Python<'py>,
    module: Bound<'py, PyAny>,
    recording: Rc<RefCell<Recording>>,
    baseline: Option<Py<PyAny>>,
    variant: Option<Py<PyAny>>,
    decorated: bool,
    memos: HashMap<(bool, usize, String), Outcome>,
    dropped: Vec<String>,
}
impl Guest for PythonGuest<'_> {
    fn call_count(&self) -> usize {
        self.recording.borrow().calls.len()
    }
    fn dropped(&self) -> &[String] {
        &self.dropped
    }
    fn compile(
        &mut self,
        source: &str,
        mutated: &str,
        qualname: &str,
        decorated: bool,
    ) -> Result<(), String> {
        self.decorated = decorated;
        self.baseline = Some(
            compile(self.py, &self.module, source, qualname, decorated)
                .map_err(|e| e.to_string())?
                .unbind(),
        );
        self.variant = Some(
            compile(self.py, &self.module, mutated, qualname, decorated)
                .map_err(|e| e.to_string())?
                .unbind(),
        );
        Ok(())
    }
    fn compare(
        &mut self,
        call: usize,
        plan: Option<(&str, f64, bool)>,
        control: bool,
        memo: bool,
    ) -> Result<Option<f64>, String> {
        self.compare_py(call, plan, control, memo)
            .map_err(|e| e.to_string())
    }
}
impl PythonGuest<'_> {
    fn compare_py(
        &mut self,
        call: usize,
        plan: Option<(&str, f64, bool)>,
        control: bool,
        memo: bool,
    ) -> PyResult<Option<f64>> {
        let py = self.py;
        let recording = self.recording.borrow();
        let (args, kwargs) = &recording.calls[call];
        let (args, kwargs) = (args.bind(py), kwargs.bind(py));
        let (args, kwargs) = if let Some((_, factor, params)) = plan {
            let (args, kwargs, changed) = transform(args, kwargs, factor, params)?;
            if !changed {
                return Ok(None);
            }
            (args, kwargs)
        } else {
            (args.clone(), kwargs.clone())
        };
        let key = (
            self.decorated,
            call,
            plan.map_or("rec", |p| p.0).to_string(),
        );
        let cached = if memo {
            self.memos.get(&key).map(|v| v.copy(py))
        } else {
            None
        };
        let variant_args = match clone_value(&args) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let variant_kwargs = match clone_value(&kwargs) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let expected = if let Some(value) = cached {
            value
        } else {
            let baseline_args = match clone_value(&args) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            };
            let baseline_kwargs = match clone_value(&kwargs) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            };
            let outcome = invoke(
                py,
                self.baseline.as_ref().unwrap().bind(py),
                &baseline_args,
                &baseline_kwargs,
            )?;
            if memo {
                self.memos.insert(key, outcome.copy(py));
            }
            outcome
        };
        let function = if control {
            self.baseline.as_ref().unwrap()
        } else {
            self.variant.as_ref().unwrap()
        };
        let actual = invoke(py, function.bind(py), &variant_args, &variant_kwargs)?;
        match (expected, actual) {
            (Outcome::Raised(a), Outcome::Raised(b)) if a.bind(py).is(b.bind(py)) => Ok(None),
            (Outcome::Raised(_), _) | (_, Outcome::Raised(_)) => Ok(Some(f64::INFINITY)),
            (Outcome::Value(a), Outcome::Value(b)) => {
                let absolute = difference(a.bind(py), b.bind(py))?;
                if absolute == 0.0 || absolute == f64::INFINITY {
                    Ok(Some(absolute))
                } else {
                    Ok(Some(absolute / magnitude(a.bind(py))?.max(1e-30)))
                }
            }
        }
    }
}

type Restore = (Py<PyAny>, String, Py<PyAny>);
fn sites<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    qualname: &str,
) -> PyResult<Vec<(Bound<'py, PyAny>, String)>> {
    let mut parts: Vec<&str> = qualname.split('.').collect();
    let name = parts
        .pop()
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("empty qualname"))?;
    let mut owner = module.clone();
    for part in parts {
        owner = owner.getattr(part)?;
    }
    let original = owner.getattr(name)?;
    let mut sites = vec![(owner.clone(), name.to_owned())];
    if owner.is(module) {
        let modules = py
            .import("sys")?
            .getattr("modules")?
            .cast_into::<PyDict>()?;
        let values = modules.values();
        for entry in values.try_iter()? {
            let entry = entry?;
            if entry.is_none() || entry.is(module) {
                continue;
            }
            let Ok(value) = entry.getattr(name) else {
                continue;
            };
            let alias = value.is(&original)
                || value
                    .getattr("__equivalence_recorder__")
                    .and_then(|x| x.is_truthy())
                    .unwrap_or(false);
            if alias {
                sites.push((entry, name.to_owned()));
            }
        }
    }
    Ok(sites)
}
fn install<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    qualname: &str,
) -> PyResult<(Vec<Restore>, Rc<RefCell<Recording>>)> {
    let sites = sites(py, module, qualname)?;
    let (owner, name) = &sites[0];
    let raw = py
        .import("inspect")?
        .call_method1("getattr_static", (owner, name))?;
    let builtins = py.import("builtins")?;
    let original = if raw.is_instance(&builtins.getattr("staticmethod")?)? {
        raw.getattr("__func__")?
    } else {
        owner.getattr(name)?
    };
    let recording = Rc::new(RefCell::new(Recording::default()));
    let original_for_wrapper = original.clone();
    let recorder = Py::new(
        py,
        Recorder {
            target: original.unbind(),
            recording: recording.clone(),
        },
    )?;
    py.import("functools")?
        .call_method1("update_wrapper", (recorder.bind(py), original_for_wrapper))?;
    let mut restores: Vec<Restore> = Vec::new();
    for (owner, name) in sites {
        let original = match owner.getattr(&name) {
            Ok(value) => value,
            Err(error) => {
                restore_all(py, restores)?;
                return Err(error);
            }
        };
        if let Err(error) = owner.setattr(&name, recorder.bind(py)) {
            restore_all(py, restores)?;
            return Err(error);
        }
        restores.push((owner.unbind(), name, original.unbind()));
    }
    Ok((restores, recording))
}
fn restore_all(py: Python<'_>, restores: Vec<Restore>) -> PyResult<()> {
    let mut last_error = None;
    for (owner, name, value) in restores.into_iter().rev() {
        if let Err(error) = owner.bind(py).setattr(name, value.bind(py)) {
            last_error = Some(error);
        }
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
fn record_many<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    names: &[String],
    tests: &[String],
) -> PyResult<HashMap<String, Rc<RefCell<Recording>>>> {
    let mut out = HashMap::new();
    for chunk in names.chunks(32) {
        let mut restores: Vec<Restore> = Vec::new();
        for name in chunk {
            match install(py, module, name) {
                Ok((sites, recording)) => {
                    restores.extend(sites);
                    out.insert(name.clone(), recording);
                }
                Err(error)
                    if error.is_instance_of::<pyo3::exceptions::PyAttributeError>(py)
                        || error.is_instance_of::<pyo3::exceptions::PyLookupError>(py)
                        || error.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py) => {}
                Err(error) => {
                    restore_all(py, restores)?;
                    return Err(error);
                }
            }
        }
        if restores.is_empty() {
            continue;
        }
        let mut args = vec![
            "-q".to_string(),
            "-p".into(),
            "no:cacheprovider".into(),
            "-o".into(),
            "addopts=".into(),
        ];
        args.extend_from_slice(tests);
        let result = py
            .import("pytest")
            .and_then(|pytest| pytest.call_method1("main", (args,)));
        restore_all(py, restores)?;
        result?;
    }
    Ok(out)
}
/// Guest child entry point. The caller must supply a private candidate snapshot and
/// an explicitly authorized Python host; no source is edited by this entry point.
#[pyfunction]
#[pyo3(signature = (module_path, module_name, tests, names, noise=1e-6, budget_seconds=30.0, extra_rules=None))]
// The interpreter token is implicit in Python; retain the seven named guest options.
#[allow(clippy::too_many_arguments)]
pub fn equivalence_probe_guest(
    py: Python<'_>,
    module_path: &str,
    module_name: &str,
    tests: Vec<String>,
    names: Vec<String>,
    noise: f64,
    budget_seconds: f64,
    extra_rules: Option<Vec<String>>,
) -> PyResult<String> {
    let source = crate::detector_scan::read_source(std::path::Path::new(module_path))
        .map_err(pyo3::exceptions::PyIOError::new_err)?;
    let rules = crate::ablation::select(None, extra_rules.as_deref().unwrap_or_default())
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let ablations = crate::ablation::collect(&source, &rules)
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let module = py
        .import("importlib")?
        .call_method1("import_module", (module_name,))?;
    let recordings = record_many(py, &module, &names, &tests)?;
    let mut rows = Vec::new();
    for name in names {
        let relevant: Vec<_> = ablations
            .iter()
            .filter(|a| a.qualname == name)
            .cloned()
            .collect();
        if relevant.is_empty() {
            continue;
        }
        let recording = recordings.get(&name).cloned().unwrap_or_default();
        let dropped = recording.borrow().dropped.clone();
        let mut guest = PythonGuest {
            py,
            module: module.clone(),
            recording,
            baseline: None,
            variant: None,
            decorated: false,
            memos: HashMap::new(),
            dropped,
        };
        rows.extend(
            equivalence_runtime::probe(&source, &relevant, &mut guest, noise, Some(budget_seconds))
                .map_err(pyo3::exceptions::PyRuntimeError::new_err)?,
        );
    }
    serde_json::to_string(&rows)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

/// Thin CPython entry point: argument interpretation and all probing remain Rust.
#[pyfunction]
pub fn equivalence_probe_guest_cli(py: Python<'_>) -> PyResult<()> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let module = argv
        .get(1)
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing module"))?;
    let mut tests = Vec::new();
    let mut names = Vec::new();
    let mut report = None;
    let mut index = 2;
    while index < argv.len() {
        match argv[index].as_str() {
            "--function" | "--json" => {
                let value = argv.get(index + 1).ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err("missing option value")
                })?;
                if argv[index] == "--function" {
                    names.push(value.clone());
                } else {
                    report = Some(value.clone());
                }
                index += 2;
            }
            value if value.starts_with('-') => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown option {value}"
                )))
            }
            _ => {
                tests.push(argv[index].clone());
                index += 1;
            }
        }
    }
    if tests.is_empty() || names.is_empty() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "missing tests or function",
        ));
    }
    let dotted = module
        .strip_suffix(".py")
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("module must end in .py"))?
        .replace('/', ".");
    let text = equivalence_probe_guest(py, module, &dotted, tests, names, 1e-6, 30.0, None)?;
    let report = report.ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing report"))?;
    std::fs::write(report, format!("{text}\n"))
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))
}

#[cfg(test)]
mod guest_tests {
    use super::*;
    #[test]
    fn guest_records_pytest_tensors_and_measures_real_ablations() {
        Python::initialize();
        let root =
            std::env::temp_dir().join(format!("forge-equivalence-guest-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/equivalence_guest");
        for name in [
            "equivalence_guest_fixture.py",
            "test_equivalence_guest_fixture.py",
        ] {
            std::fs::copy(fixtures.join(name), root.join(name)).unwrap();
        }
        Python::attach(|py| {
            let sys = py.import("sys").unwrap();
            sys.getattr("path")
                .unwrap()
                .call_method1("insert", (0, root.to_str().unwrap()))
                .unwrap();
            let module = root.join("equivalence_guest_fixture.py");
            let test = root.join("test_equivalence_guest_fixture.py");
            let text = equivalence_probe_guest(
                py,
                module.to_str().unwrap(),
                "equivalence_guest_fixture",
                vec![test.to_str().unwrap().to_string()],
                vec!["load_bearing".into(), "inert".into(), "Lane.forward".into()],
                1e-6,
                30.0,
                None,
            )
            .unwrap();
            let rows: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
            assert!(rows.iter().any(|r| r["qualname"] == "Lane.forward"
                && r["rule"] == "drop_clamp_min"
                && r["verdict"] == "LIVE"));
            assert!(rows.iter().any(|r| r["qualname"] == "load_bearing"
                && r["rule"] == "drop_clamp_min"
                && r["verdict"] == "LIVE"));
            assert!(rows.iter().any(|r| r["qualname"] == "inert"
                && r["rule"] == "drop_clamp_min"
                && r["verdict"] == "NO_DIFFERENCE_OBSERVED"));
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}
