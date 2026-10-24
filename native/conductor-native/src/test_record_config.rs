//! `[tool.forge.test_record]`: whether, and where, a host project records its test
//! runs.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    /// The SQLite record: absolute, `~/`-rooted, or relative to the declaring directory.
    store: String,
    /// Per-line hits feed coverage. Off records executed files only.
    #[serde(default = "enabled")]
    lines: bool,
    /// Environment variables whose values a test result depends on.
    #[serde(default)]
    environment: Vec<String>,
}

fn enabled() -> bool {
    true
}

/// The nearest `pyproject.toml` at or above `start` that declares the table. A
/// nested package's own pyproject does not stop the walk: the host repository's
/// table governs every run inside it.
fn find(start: &Path) -> Result<Option<(PathBuf, Table)>, String> {
    for dir in start.ancestors() {
        let path = dir.join("pyproject.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let doc: toml::Value =
            toml::from_str(&raw).map_err(|error| format!("{}: {error}", path.display()))?;
        let Some(table) = doc
            .get("tool")
            .and_then(|tool| tool.get("forge"))
            .and_then(|forge| forge.get("test_record"))
        else {
            continue;
        };
        let table = table
            .clone()
            .try_into::<Table>()
            .map_err(|error| format!("{} [tool.forge.test_record]: {error}", path.display()))?;
        return Ok(Some((dir.to_path_buf(), table)));
    }
    Ok(None)
}

fn store_path(root: &Path, store: &str, home: Option<&str>) -> Result<PathBuf, String> {
    match store.strip_prefix("~/") {
        Some(rest) => home
            .map(|home| Path::new(home).join(rest))
            .ok_or_else(|| format!("store {store:?} needs HOME")),
        None => Ok(root.join(store)),
    }
}

/// `(root, store, lines, environment)` as Python receives it.
type Resolved = (String, String, bool, Vec<String>);

/// The resolved table, or `None` when no pyproject declares one.
#[pyfunction]
pub(crate) fn test_record_config_native(start_dir: &str) -> PyResult<Option<Resolved>> {
    let start = std::fs::canonicalize(start_dir)
        .map_err(|error| PyValueError::new_err(format!("{start_dir}: {error}")))?;
    let Some((root, table)) = find(&start).map_err(PyValueError::new_err)? else {
        return Ok(None);
    };
    let home = std::env::var("HOME").ok();
    let store = store_path(&root, &table.store, home.as_deref()).map_err(PyValueError::new_err)?;
    Ok(Some((
        root.to_string_lossy().into_owned(),
        store.to_string_lossy().into_owned(),
        table.lines,
        table.environment,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_declaring_pyproject_wins_and_unknown_keys_fail() {
        let root = std::env::temp_dir().join(format!("test-record-config-{}", std::process::id()));
        let nested = root.join("pkg");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.forge.test_record]\nstore = \".record/r.sqlite\"\nenvironment = [\"X\"]\n",
        )
        .unwrap();
        std::fs::write(nested.join("pyproject.toml"), "[project]\nname = \"pkg\"\n").unwrap();
        let (found, table) = find(&nested).unwrap().unwrap();
        assert_eq!(found, root);
        assert!(table.lines);
        assert_eq!(table.environment, ["X"]);
        assert_eq!(
            store_path(&found, &table.store, None).unwrap(),
            root.join(".record/r.sqlite")
        );
        assert_eq!(
            store_path(&found, "~/r.sqlite", Some("/home/x")).unwrap(),
            Path::new("/home/x/r.sqlite")
        );
        // One store shared by every checkout and clone of the repository.
        assert_eq!(
            store_path(&found, "/srv/record/r.sqlite", None).unwrap(),
            Path::new("/srv/record/r.sqlite")
        );
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.forge.test_record]\nstore = \"r\"\nstroe = \"typo\"\n",
        )
        .unwrap();
        assert!(find(&nested).unwrap_err().contains("stroe"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
