//! Fixtures and independent Python-AST references for the repository index contracts.

use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn fixture(case: &Case) -> &Path {
    case.write("pkg/__init__.py", "");
    case.write("pkg/target.py", "");
    case.write("pkg/direct.py", "");
    case.write("pkg/multiple.py", "");
    case.write("pkg/from_module.py", "def thing():\n    pass\n");
    case.write("pkg/from_package.py", "");
    case.write("pkg/nested.py", "");
    case.write("pkg/sub/__init__.py", "");
    case.write("pkg/sub/local.py", "");
    case.write(
        "pkg/test_imports.py",
        r#"import pkg.direct as direct_alias, pkg.multiple as multiple_alias
from pkg.from_module import thing
from pkg import from_package as imported_target

def nested():
    from pkg import nested
    return "import pkg.quoted_only"

# from pkg import quoted_only
"#,
    );
    case.write(
        "pkg/sub/test_relative.py",
        "from .. import target\nfrom . import local\n",
    );
    case.write(
        "pkg/test_words.py",
        "# refine_unexercised is indirect\nname = 'refine_unexercised'\n",
    );
    case.write(
        "pkg/test_suffix_only.py",
        "def refine_unexercised_extra():\n    pass\n",
    );
    case.write("pkg/test_broken.py", "def broken(:\n    hidden_helper\n");
    case.root()
}

pub fn expected_tests() -> BTreeSet<String> {
    [
        "pkg/sub/test_relative.py",
        "pkg/test_broken.py",
        "pkg/test_imports.py",
        "pkg/test_suffix_only.py",
        "pkg/test_words.py",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src")
        .canonicalize()
        .expect("resolve Forge Python source root");
    assert!(root.join("conductor/repo_index.py").is_file());
    assert!(root.join("conductor/slop_gate.py").is_file());
    root
}

pub fn tests_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("read test inventory directory") {
            let entry = entry.expect("read test inventory entry");
            let name = entry.file_name();
            let kind = entry.file_type().expect("read test inventory type");
            if kind.is_dir() {
                if name != ".git" && name != ".venv" {
                    dirs.push(entry.path());
                }
            } else if kind.is_file() {
                let name = name.to_string_lossy();
                if name.starts_with("test_") && name.ends_with(".py") {
                    files.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
                }
            }
        }
    }
    files.sort();
    files
}

pub fn index<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    module(py, "conductor.repo_index")
        .getattr("build")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
}

pub fn paths(index: &Bound<'_, PyAny>, method: &str, name: &str) -> BTreeSet<String> {
    index
        .call_method1(method, (name,))
        .unwrap()
        .extract::<Vec<String>>()
        .unwrap()
        .into_iter()
        .collect()
}

/// The retired Python test's old matcher, driven by the actual Python `ast` module.
pub fn old_matcher_targets(ast: &Bound<'_, PyModule>, source: &str) -> PyResult<BTreeSet<String>> {
    let tree = ast.call_method1("parse", (source,))?;
    let from_type = ast.getattr("ImportFrom")?;
    let import_type = ast.getattr("Import")?;
    let mut targets = BTreeSet::new();
    for node in ast.call_method1("walk", (tree,))?.try_iter()? {
        let node = node?;
        if node.is_instance(&from_type)? {
            let module: Option<String> = node.getattr("module")?.extract()?;
            let level: usize = node.getattr("level")?.extract()?;
            if level == 0 {
                if let Some(module) = module {
                    targets.insert(module);
                }
            }
        } else if node.is_instance(&import_type)? {
            for alias in node.getattr("names")?.try_iter()? {
                targets.insert(alias?.getattr("name")?.extract()?);
            }
        }
    }
    Ok(targets)
}

pub fn cli(py: Python<'_>, args: &[String]) -> (i64, String) {
    let io = PyModule::import(py, "io").unwrap();
    let stdout = io.call_method0("StringIO").unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let _patch = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
    let status = module(py, "conductor.repo_index")
        .getattr("main")
        .unwrap()
        .call1((args.to_vec(),))
        .unwrap()
        .extract()
        .unwrap();
    let output = stdout.call_method0("getvalue").unwrap().extract().unwrap();
    (status, output)
}
