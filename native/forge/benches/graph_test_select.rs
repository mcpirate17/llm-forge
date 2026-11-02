//! Current-only, warm in-process advisory selection. No baseline speedup claim.
use anyhow::Result;
use git2::{Repository, Signature};
use serde_json::json;
use std::{fs, hint::black_box, time::Instant};

fn main() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let repository = Repository::init(&root)?;
    fs::write(root.join("base.py"), "BASE = 0\n")?;
    let mut index = repository.index()?;
    index.add_path(std::path::Path::new("base.py"))?;
    index.write()?;
    let tree = repository.find_tree(index.write_tree()?)?;
    let signature = Signature::now("Benchmark", "benchmark@example.invalid")?;
    repository.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])?;
    fs::create_dir(root.join("tests"))?;
    for index in 0..64 {
        fs::write(
            root.join(format!("tests/test_{index}.py")),
            "def test_one(): pass\n",
        )?;
    }
    fs::create_dir(root.join(".code-review-graph"))?;
    rusqlite::Connection::open(root.join(".code-review-graph/graph.db"))?.execute_batch(
        "CREATE TABLE nodes(qualified_name TEXT, file_path TEXT, is_test INTEGER);
        CREATE TABLE edges(source_qualified TEXT, target_qualified TEXT);",
    )?;
    let mut discovery = Vec::new();
    let mut planning = Vec::new();
    for sample in 0..12 {
        let started = Instant::now();
        let sources = forge::graph_test_select::changed_files(&root)?;
        let discovered = started.elapsed().as_secs_f64() * 1e6;
        let started = Instant::now();
        let plan = forge::graph_test_select::select(&root, &sources)?;
        let planned = started.elapsed().as_secs_f64() * 1e6;
        anyhow::ensure!(
            !plan.complete && plan.selected_tests.len() == 64,
            "invalid fixture selection"
        );
        black_box(plan);
        if sample > 0 {
            discovery.push(discovered);
            planning.push(planned);
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scope":"current-only in-process; warm filesystem cache; unisolated host; no baseline speedup claim",
            "fixture":"64 untracked Python tests, empty legacy SQLite graph, complete inventory fallback",
            "samples":11,"warmup":1,"discovery_us":discovery,"selection_us":planning,
            "excludes":"fixture setup, process startup and final output serialization"
        }))?
    );
    Ok(())
}
