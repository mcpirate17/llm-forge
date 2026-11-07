//! Bind automatic module selection to the already bounded captured input tree.
use super::{source::SelectedSource, GateArgs};
use crate::candidate_source::{LiveSelection, LiveSelectionMode};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate(args: &GateArgs) -> Result<()> {
    let modes = usize::from(!args.module.is_empty())
        + usize::from(args.changed_from.is_some())
        + usize::from(args.all_tracked);
    ensure!(
        modes == 1,
        "select explicit modules, changed-from, or all-tracked"
    );
    ensure!(args.module.len() <= 100_000, "module count exceeds 100000");
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for module in &args.module {
        crate::candidate_source::safe_path(module.as_bytes())?;
        ensure!(
            module.ends_with(".py") && !module.chars().any(char::is_control),
            "module must be a literal relative Python source path"
        );
        ensure!(seen.insert(module), "module is repeated: {module}");
        total += module.len();
        ensure!(total <= 64 * 1024 * 1024, "module paths exceed 64 MiB");
    }
    Ok(())
}

fn production_python(path: &str) -> bool {
    let mut components = path.rsplit('/');
    let Some(name) = components.next() else {
        return false;
    };
    name.ends_with(".py")
        && !name.starts_with("test_")
        && name != "conftest.py"
        && !components.any(|part| matches!(part, "test" | "tests"))
}

pub(super) struct SelectedModules {
    paths: Vec<String>,
    binding: Option<LiveSelection>,
}
impl SelectedModules {
    pub(super) fn capture(args: &GateArgs, selected: &SelectedSource) -> Result<Self> {
        selected.verify()?;
        let mode = if let Some(base) = &args.changed_from {
            Some(LiveSelectionMode::ChangedFrom(base.clone()))
        } else if args.all_tracked {
            Some(LiveSelectionMode::AllTracked)
        } else {
            None
        };
        let Some(mode) = mode else {
            return Ok(Self {
                paths: args.module.clone(),
                binding: None,
            });
        };
        let binding =
            selected
                .origin()
                .select_live_paths(mode, selected.tree_source(), selected.tree())?;
        let entries = selected.tree_source().list_tree(selected.tree())?;
        let modes: BTreeMap<_, _> = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.mode.as_str()))
            .collect();
        let mut paths = Vec::new();
        for path in binding
            .paths()
            .iter()
            .filter(|path| production_python(path))
        {
            let Some(mode) = modes.get(path.as_str()) else {
                continue;
            };
            ensure!(
                matches!(*mode, "100644" | "100755"),
                "automatic selection requires an ordinary Python module: {path} (mode {mode})"
            );
            paths.push(path.clone());
        }
        let result = Self {
            paths,
            binding: Some(binding),
        };
        result.verify(selected)?;
        Ok(result)
    }
    pub(super) fn paths(&self) -> &[String] {
        &self.paths
    }
    pub(super) fn module(&self) -> Option<&str> {
        if let [module] = self.paths.as_slice() {
            Some(module)
        } else {
            None
        }
    }
    pub(super) fn verify(&self, selected: &SelectedSource) -> Result<()> {
        selected.verify()?;
        if let Some(binding) = &self.binding {
            binding.verify(selected.origin(), selected.tree_source(), selected.tree())?;
        }
        Ok(())
    }
    pub(super) fn evidence(&self) -> Value {
        json!({"schema_version":1, "mode":if self.binding.is_some() { "automatic" } else { "explicit" },
            "modules":self.paths, "module_count":self.paths.len(),
            "binding":self.binding.as_ref().map(LiveSelection::evidence),
            "scope":"whole_modules_from_captured_inputs", "approval":false})
    }
}
