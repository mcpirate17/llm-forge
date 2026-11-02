//! Advisory changed-code test selection. Never produces a governance PASS receipt.
//! The graph core owns dependency closure and conservative inventory fallback.
use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[path = "graph_test_select_contracts.rs"]
mod contracts;
#[path = "graph_test_select_run.rs"]
mod execution;
pub use execution::{worker, WorkerArgs};
#[cfg(test)]
#[path = "graph_test_select_tests.rs"]
mod tests;

#[derive(Args, Debug)]
pub struct SelectArgs {
    /// Source paths; default: staged, unstaged, deleted and untracked Git files.
    pub paths: Vec<String>,
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Execute under the native supervisor, only after all prerequisites validate.
    #[arg(long, conflicts_with = "json")]
    pub run: bool,
    #[arg(long)]
    pub json: bool,
    /// Explicit external pytest/Python compatibility guest interpreter.
    #[arg(long)]
    pub python: Option<PathBuf>,
    /// Existing private directory outside the checkout for contract build scratch.
    #[arg(long)]
    pub runtime_dir: Option<PathBuf>,
    /// Repeat this option for each pytest argument, e.g. --pytest-arg=-q.
    #[arg(long = "pytest-arg", allow_hyphen_values = true)]
    pub pytest_args: Vec<String>,
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct Selection {
    pub scope: String,
    pub complete: bool,
    pub fallback_reasons: Vec<String>,
    pub generation: Option<String>,
    pub metadata: Value,
    pub unmapped_sources: Vec<String>,
    pub sources: Vec<String>,
    /// Full graph result, including non-Python tests. Never silently discarded.
    pub selected_paths: Vec<String>,
    pub selected_tests: Vec<String>,
    pub contract_targets: Vec<String>,
    pub contract_test_paths: Vec<String>,
    pub native_manifests: Vec<PathBuf>,
    pub unsupported_tests: Vec<String>,
    pub count: usize,
}

pub fn repository_root(path: &Path) -> Result<PathBuf> {
    let repository = git2::Repository::discover(path).context("discover Git repository")?;
    repository
        .workdir()
        .context("bare repositories cannot select tests")?
        .canonicalize()
        .context("resolve Git work directory")
}

/// libgit2 paths preserve whitespace and newlines without line-based parsing.
/// Rename detection stays off, retaining both deleted and newly added paths.
pub fn changed_files(root: &Path) -> Result<Vec<String>> {
    let repository = git2::Repository::open(root).context("open Git repository")?;
    repository
        .head()
        .context("changed test discovery requires an existing Git HEAD")?;
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(false)
        .renames_index_to_workdir(false);
    let statuses = repository
        .statuses(Some(&mut options))
        .context("read Git changes")?;
    ensure!(
        statuses.len() <= 1024,
        "Git change inventory exceeds 1024 paths; select explicit sources"
    );
    let mut paths = BTreeSet::new();
    for entry in statuses.iter() {
        let raw = std::str::from_utf8(entry.path_bytes()).context("Git path is not UTF-8")?;
        let path = normalize(root, raw)?;
        ensure!(
            !root.join(&path).is_dir(),
            "changed Git path is a directory or submodule: {path}"
        );
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

fn normalize(root: &Path, raw: &str) -> Result<String> {
    ensure!(
        raw.len() <= 4096 && !raw.is_empty(),
        "source path must contain 1..4096 bytes"
    );
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.strip_prefix(root)
            .context("source path is outside repository")?
    } else {
        path
    };
    ensure!(
        !path.components().any(|part| matches!(
            part,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )),
        "source path escapes repository: {raw}"
    );
    let parts: Vec<_> = path
        .components()
        .filter(|part| *part != Component::CurDir)
        .collect();
    ensure!(!parts.is_empty(), "source path cannot be repository root");
    let normalized: PathBuf = parts.iter().collect();
    Ok(normalized
        .to_str()
        .context("source path is not UTF-8")?
        .to_owned())
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>> {
    value[key]
        .as_array()
        .with_context(|| format!("graph response missing {key}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .with_context(|| format!("graph {key} contains a non-string"))
        })
        .collect()
}

fn conventions(root: &Path, source: &str) -> BTreeSet<String> {
    let path = Path::new(source);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let parent = path.parent().unwrap_or(Path::new(""));
    let mut candidates = vec![
        parent.join(format!("test_{stem}.py")),
        parent.join("tests").join(format!("test_{stem}.py")),
        parent.join(format!("{stem}_test.py")),
        Path::new("tests").join(format!("test_{stem}.py")),
    ];
    if let Some(first) = path.components().next() {
        candidates.push(
            PathBuf::from(first.as_os_str())
                .join("tests")
                .join(format!("test_{stem}.py")),
        );
    }
    if path.extension().is_some_and(|ext| ext == "py")
        && (stem.starts_with("test_") || stem.ends_with("_test"))
    {
        candidates.push(path.to_path_buf());
    }
    candidates
        .into_iter()
        .filter(|candidate| root.join(candidate).is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
        .collect()
}

/// Reuse the common graph algorithm; no candidate-gate policy or receipts enter
/// this standalone advisory flow. Incomplete evidence keeps every graph test.
pub fn select(root: &Path, sources: &[String]) -> Result<Selection> {
    ensure!(
        sources.len() <= 1024,
        "test selection accepts at most 1024 source paths"
    );
    let sources = sources
        .iter()
        .map(|source| normalize(root, source))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let graph = if sources.is_empty() {
        json!({"paths": [], "complete": true, "scope": "no-source-changes",
            "reasons": [], "generation": null, "metadata": {}, "unmapped": []})
    } else {
        conductor_native::graph_context::dispatch(
            "test_selection",
            &json!({"repo": root, "paths": sources}),
        )
        .map_err(anyhow::Error::msg)?
    };
    let complete = graph["complete"]
        .as_bool()
        .context("graph response missing complete")?;
    let mut selected: BTreeSet<String> = strings(&graph, "paths")?.into_iter().collect();
    for source in &sources {
        selected.extend(conventions(root, source));
    }
    let mut contract_sources = sources.clone();
    // A graph-selected conductor unit-test file also requires the explicit
    // compatibility runtime, even when only its Python caller changed.
    if let Some(path) = selected
        .iter()
        .find(|path| path.starts_with("native/conductor-native/") && path.ends_with(".rs"))
    {
        contract_sources.push(path.clone());
    }
    let contracts = contracts::plan(root, &contract_sources, complete)?;
    selection(root, sources, graph, selected, contracts)
}

fn selection(
    root: &Path,
    sources: Vec<String>,
    graph: Value,
    selected: BTreeSet<String>,
    contracts: contracts::Contracts,
) -> Result<Selection> {
    let mut python = Vec::new();
    let mut manifests = BTreeSet::new();
    let mut unsupported = Vec::new();
    for raw in &selected {
        let path = normalize(root, raw)?;
        ensure!(
            root.join(&path).is_file(),
            "selected test is missing: {path}"
        );
        ensure!(
            root.join(&path).canonicalize()?.starts_with(root),
            "selected test resolves outside repository: {path}"
        );
        if path.ends_with(".py") {
            python.push(path);
        } else if contracts.test_paths.contains(&path) {
            continue;
        } else if path.ends_with(".rs") {
            let mut directory = root
                .join(&path)
                .parent()
                .context("test has no parent")?
                .to_path_buf();
            loop {
                if directory.join("Cargo.toml").is_file() {
                    manifests.insert(directory.join("Cargo.toml"));
                    break;
                }
                if directory == root || !directory.pop() {
                    unsupported.push(path);
                    break;
                }
            }
        } else {
            unsupported.push(path);
        }
    }
    let count = selected.len()
        + contracts
            .test_paths
            .iter()
            .filter(|path| !selected.contains(*path))
            .count();
    Ok(Selection {
        scope: graph["scope"]
            .as_str()
            .context("graph response missing scope")?
            .to_owned(),
        complete: graph["complete"]
            .as_bool()
            .context("graph response missing complete")?,
        fallback_reasons: strings(&graph, "reasons")?,
        generation: graph["generation"].as_str().map(str::to_owned),
        metadata: graph["metadata"].clone(),
        unmapped_sources: strings(&graph, "unmapped")?,
        sources,
        selected_paths: selected.into_iter().collect(),
        selected_tests: python,
        contract_targets: contracts.targets,
        contract_test_paths: contracts.test_paths,
        native_manifests: manifests.into_iter().collect(),
        unsupported_tests: unsupported,
        count,
    })
}

pub fn run(args: SelectArgs) -> Result<u8> {
    let root = repository_root(&args.repo)?;
    let sources = if args.paths.is_empty() {
        changed_files(&root)?
    } else {
        args.paths.clone()
    };
    let plan = select(&root, &sources)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(0);
    }
    if !plan.complete {
        eprintln!(
            "advisory full inventory fallback: {}",
            plan.fallback_reasons.join("; ")
        );
    }
    if !plan.unmapped_sources.is_empty() {
        eprintln!(
            "advisory sources with no dependency mapping: {}",
            plan.unmapped_sources.join(", ")
        );
    }
    if args.run {
        return execution::execute(&root, &plan, &args);
    }
    for path in &plan.selected_tests {
        println!("{path}");
    }
    for target in &plan.contract_targets {
        println!("cargo:{target}");
    }
    for manifest in &plan.native_manifests {
        println!("cargo-manifest:{}", manifest.display());
    }
    for path in &plan.unsupported_tests {
        println!("unsupported:{path}");
    }
    if !plan.unsupported_tests.is_empty() {
        bail!("selected test languages have no configured runner; see --json");
    }
    Ok(0)
}
