//! Full path/hash inventory of the selected relocation module family, not a build receipt.
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
const BASE: &str = "ef50dcb5629bcffab02683319c45b7a7683e9bc3";
const DIRECTORY: &str = "native/forge/src";
fn git(repo: &Path, tool: &Path, scratch: &Path, argv: &[&str], tag: &str) -> Result<Vec<u8>> {
    let mut c = crate::command(tool, scratch);
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager", "-C"])
        .arg(repo)
        .args(argv);
    Ok(
        crate::measured::run(&mut c, scratch, tag, Duration::from_secs(15))?
            .require_success()?
            .stdout,
    )
}
fn selected(path: &Path) -> bool {
    path.file_name()
        .and_then(|p| p.to_str())
        .is_some_and(|p| p.starts_with("worktree_runtime") && p.ends_with(".rs"))
}
pub fn snapshot(repo: &Path, tool: &Path, scratch: &Path) -> Result<Value> {
    let listing = git(
        repo,
        tool,
        scratch,
        &["ls-tree", "-r", "--name-only", BASE, "--", DIRECTORY],
        "baseline-paths",
    )?;
    let baseline: BTreeSet<PathBuf> = String::from_utf8(listing)?
        .lines()
        .map(PathBuf::from)
        .filter(|p| selected(p))
        .collect();
    ensure!(
        !baseline.is_empty() && baseline.len() <= 128,
        "selected baseline source count invalid"
    );
    let mut current = BTreeSet::new();
    for (index, entry) in fs::read_dir(repo.join(DIRECTORY))?.enumerate() {
        ensure!(index < 8192, "source directory entry bound exceeded");
        let entry = entry?;
        if selected(&entry.path()) {
            ensure!(
                entry.file_type()?.is_file(),
                "selected source must be regular, not symlink"
            );
            current.insert(Path::new(DIRECTORY).join(entry.file_name()));
        }
    }
    ensure!(
        !current.is_empty() && current.len() <= 128,
        "selected current source count invalid"
    );
    let mut rows = Vec::new();
    let mut total = 0usize;
    for path in baseline.union(&current) {
        let text = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 source path"))?;
        let old = if baseline.contains(path) {
            Some(git(
                repo,
                tool,
                scratch,
                &["cat-file", "blob", &format!("{BASE}:{text}")],
                "baseline-source",
            )?)
        } else {
            None
        };
        let new = if current.contains(path) {
            ensure!(
                fs::metadata(repo.join(path))?.len() <= 2 * 1024 * 1024,
                "source exceeds2MiB"
            );
            {
                let mut bytes = Vec::new();
                fs::File::open(repo.join(path))?
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                Some(bytes)
            }
        } else {
            None
        };
        for bytes in old.iter().chain(new.iter()) {
            ensure!(bytes.len() <= 2 * 1024 * 1024, "source exceeds2MiB");
            total += bytes.len();
            ensure!(total <= 64 * 1024 * 1024, "source inventory exceeds64MiB");
        }
        let hash = |bytes: &Vec<u8>| crate::fixture::hash(bytes);
        rows.push(json!({"path":text,"baseline_sha256":old.as_ref().map(hash),
            "current_sha256":new.as_ref().map(hash),"baseline_bytes":old.as_ref().map(Vec::len),
            "current_bytes":new.as_ref().map(Vec::len),"changed":old != new}));
    }
    Ok(
        json!({"schema":"forge-relocation-source-inventory-v1","baseline_ref":BASE,
        "selection":"all immediate native/forge/src/worktree_runtime*.rs files, union of baseline and current; excludes unrelated modules and is not complete shipping fingerprint",
        "files":rows,"contract_requirement":"retain fsync counts, atomic exchange/publication and refusal/identity safeguards; independent source review required"}),
    )
}
