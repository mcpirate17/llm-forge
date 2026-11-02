//! Benchmark-owned Git data snapshots. Never a linked developer tree or clone.
use crate::{
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::{Kind, Source},
};
use anyhow::{ensure, Context, Result};
use conductor_native::candidate_benchmark as data;
use git2::{Index, IndexEntry, IndexTime, ObjectType, Oid, Repository, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(crate) const LEGACY: [&str; 6] = [
    "docs-only",
    "small-python",
    "native-code",
    "dependency",
    "large-delete-rename",
    "full-review",
];
pub(crate) const MANIFEST: &str = ".git/forge-benchmark.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scenario {
    pub name: String,
    pub profile: String,
    pub tree_oid: String,
    pub index_sha256: String,
    pub changes: Vec<serde_json::Value>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub schema_version: u32,
    pub workload_revision: String,
    pub source_root: PathBuf,
    pub source_kind: Kind,
    pub source_head: String,
    pub source_tree: String,
    pub baseline_commit: String,
    pub baseline_tree: String,
    pub package_root: String,
    pub scenarios: Vec<Scenario>,
}
pub(crate) fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
pub(crate) fn names(selected: &[String]) -> Result<Vec<String>> {
    ensure!(
        selected.len() <= 7,
        "at most seven unique scenarios are supported"
    );
    let names = if selected.is_empty() {
        LEGACY.iter().map(|s| (*s).into()).collect()
    } else {
        selected.to_vec()
    };
    ensure!(
        names.len() <= 7,
        "at most seven unique scenarios are supported"
    );
    let mut seen = BTreeSet::new();
    for name in &names {
        ensure!(
            LEGACY.contains(&name.as_str()) || name == "native-rust",
            "unknown benchmark scenario: {name}"
        );
        ensure!(seen.insert(name), "duplicate benchmark scenario: {name}");
    }
    Ok(names)
}
pub(crate) fn index_path(repo: &Path, scenario: &str) -> PathBuf {
    repo.join(".git")
        .join(format!("forge-benchmark-{scenario}.index"))
}
pub(crate) fn insert(
    repo: &Repository,
    index: &mut Index,
    path: &str,
    content: &[u8],
) -> Result<()> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    ensure!(
        content.len() <= 64 * 1024 * 1024,
        "benchmark blob exceeds 64 MiB"
    );
    index.add(&IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: content.len().try_into()?,
        id: repo.blob(content)?,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })?;
    Ok(())
}
/// Copy reachable immutable objects, retaining source ancestry without alternates.
fn import(repo: &Repository, source: &Repository, roots: &[Oid]) -> Result<()> {
    let input = source.odb()?;
    let output = repo.odb()?;
    let mut pending = roots.to_vec();
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    while let Some(oid) = pending.pop() {
        if !seen.insert(oid) {
            continue;
        }
        ensure!(
            seen.len() <= 100_000 && pending.len() <= 100_000,
            "benchmark reachable object inventory exceeds 100000"
        );
        let (size, kind) = input.read_header(oid)?;
        total = total
            .checked_add(size)
            .context("benchmark object bytes overflow")?;
        ensure!(
            size <= 64 * 1024 * 1024 && total <= 2 * 1024 * 1024 * 1024usize,
            "benchmark history exceeds 64 MiB/object or 2 GiB aggregate"
        );
        let raw = input.read(oid)?;
        ensure!(
            output.write(kind, raw.data())? == oid,
            "copied benchmark object changed identity"
        );
        match kind {
            ObjectType::Commit => {
                let commit = source.find_commit(oid)?;
                ensure!(
                    pending
                        .len()
                        .checked_add(commit.parent_count())
                        .is_some_and(|count| count < 100_000),
                    "benchmark history fanout exceeds limit"
                );
                pending.push(commit.tree_id());
                pending.extend(commit.parent_ids());
            }
            ObjectType::Tree => {
                let tree = source.find_tree(oid)?;
                ensure!(
                    pending.len() + tree.len() <= 100_000,
                    "benchmark tree fanout exceeds limit"
                );
                for entry in tree.iter() {
                    // Historical gitlinks name external repositories, not owned ODB objects.
                    // Current captured gitlinks are refused by Snapshot before publication.
                    if entry.kind() != Some(ObjectType::Commit) {
                        pending.push(entry.id());
                    }
                }
            }
            ObjectType::Blob => (),
            _ => anyhow::bail!("unsupported reachable benchmark object type"),
        }
    }
    Ok(())
}
/// Check the captured tree before inserting any synthetic data. An existing
/// tree at the leaf also collides: replacing its descendants is not preservation.
fn require_absent(repo: &Repository, captured: Oid, path: &str) -> Result<()> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    let mut tree = repo.find_tree(captured)?;
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let Some(entry) = tree.get_name(part).map(|entry| entry.to_owned()) else {
            return Ok(());
        };
        ensure!(
            parts.peek().is_some() && entry.kind() == Some(ObjectType::Tree),
            "source collides with generated benchmark path or its ancestor: {path}"
        );
        tree = repo.find_tree(entry.id())?;
    }
    anyhow::bail!("source collides with generated benchmark path: {path}")
}
fn reserve_generated_path(
    repo: &Repository,
    captured: Oid,
    reserved: &mut BTreeSet<String>,
    path: &str,
) -> Result<()> {
    require_absent(repo, captured, path)?;
    let ancestor = Path::new(path)
        .ancestors()
        .skip(1)
        .filter_map(|parent| parent.to_str())
        .any(|parent| reserved.contains(parent));
    let prefix = format!("{path}/");
    let descendant = reserved.contains(path)
        || reserved
            .range(prefix.clone()..)
            .next()
            .is_some_and(|other| other.starts_with(&prefix));
    ensure!(
        !ancestor && !descendant,
        "generated benchmark paths conflict: {path}"
    );
    reserved.insert(path.to_owned());
    Ok(())
}
fn admit_generated_paths(
    repo: &Repository,
    captured: Oid,
    package: &str,
    selected: &[String],
) -> Result<()> {
    let mut reserved = BTreeSet::new();
    for path in [
        data::python_path(package).map_err(anyhow::Error::msg)?,
        data::RUST_INPUT_PATH.into(),
        "native/benchmark_probe/Cargo.toml".into(),
    ] {
        reserve_generated_path(repo, captured, &mut reserved, &path)?;
    }
    for n in 0..500 {
        reserve_generated_path(
            repo,
            captured,
            &mut reserved,
            &format!("benchmark_fixture/obsolete_{n:04}.txt"),
        )?;
    }
    for scenario in selected {
        match scenario.as_str() {
            "docs-only" => reserve_generated_path(
                repo,
                captured,
                &mut reserved,
                "research/notes/candidate_review_benchmark_probe.md",
            )?,
            "native-code" => reserve_generated_path(
                repo,
                captured,
                &mut reserved,
                &format!("{package}/candidate_review/test_benchmark_probe.c"),
            )?,
            "large-delete-rename" => {
                for n in 250..500 {
                    reserve_generated_path(
                        repo,
                        captured,
                        &mut reserved,
                        &format!("benchmark_fixture/renamed_{n:04}.txt"),
                    )?;
                }
            }
            // Python/Rust scenarios edit synthetic baseline inputs already admitted above.
            // The dependency scenario intentionally edits the captured lockfile.
            _ => (),
        }
    }
    Ok(())
}
fn baseline(
    repo: &Repository,
    captured: Oid,
    parent: Oid,
    package: &str,
    selected: &[String],
) -> Result<(Oid, Oid)> {
    admit_generated_paths(repo, captured, package, selected)?;
    let mut index = repo.index()?;
    let captured_tree = repo.find_tree(captured)?;
    index.read_tree(&captured_tree)?;
    for n in 0..500 {
        insert(
            repo,
            &mut index,
            &format!("benchmark_fixture/obsolete_{n:04}.txt"),
            b"retained benchmark fixture\n",
        )?;
    }
    insert(
        repo,
        &mut index,
        &data::python_path(package).map_err(anyhow::Error::msg)?,
        data::PYTHON_BASE,
    )?;
    insert(repo, &mut index, data::RUST_INPUT_PATH, data::RUST_BASE)?;
    insert(
        repo,
        &mut index,
        "native/benchmark_probe/Cargo.toml",
        data::RUST_MANIFEST,
    )?;
    let tree = index.write_tree()?;
    index.write()?;
    let signature = Signature::new(
        "candidate-review-benchmark",
        "benchmark@example.invalid",
        &git2::Time::new(0, 0),
    )?;
    let parent = repo.find_commit(parent)?;
    let commit = repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "benchmark-owned baseline data snapshot",
        &repo.find_tree(tree)?,
        &[&parent],
    )?;
    Ok((commit, tree))
}
pub(crate) fn scenario_index(
    repo: &Repository,
    tree: Oid,
    package: &str,
    name: &str,
    path: Option<&Path>,
) -> Result<(Index, Oid)> {
    names(&[name.into()])?;
    let mut index = if let Some(path) = path {
        Index::open(path)?
    } else {
        Index::new()?
    };
    index.read_tree(&repo.find_tree(tree)?)?;
    match name {
        "docs-only" => insert(
            repo,
            &mut index,
            "research/notes/candidate_review_benchmark_probe.md",
            b"# Candidate review benchmark\n\nExact docs-only latency probe.\n",
        )?,
        "small-python" | "full-review" => {
            let (path, content) = data::python_input(package).map_err(anyhow::Error::msg)?;
            insert(repo, &mut index, &path, &content)?;
        }
        "native-code" => insert(
            repo,
            &mut index,
            &format!("{package}/candidate_review/test_benchmark_probe.c"),
            b"#include <stdint.h>\nint64_t benchmark_identity(int64_t x) { return x; }\n",
        )?,
        "native-rust" => insert(repo, &mut index, data::RUST_INPUT_PATH, data::RUST_CHANGED)?,
        "dependency" => {
            let entry = repo
                .find_tree(tree)?
                .get_path(Path::new("package-lock.json"))?;
            ensure!(
                entry.filemode() == 0o100644 || entry.filemode() == 0o100755,
                "dependency input must be a regular blob"
            );
            let blob = repo.find_blob(entry.id())?;
            ensure!(
                blob.size() < 64 * 1024 * 1024,
                "dependency fixture exceeds blob limit"
            );
            let mut content = blob.content().to_vec();
            content.push(b'\n');
            insert(repo, &mut index, "package-lock.json", &content)?;
        }
        "large-delete-rename" => {
            for n in 0..500 {
                let old = format!("benchmark_fixture/obsolete_{n:04}.txt");
                index.remove_path(Path::new(&old))?;
                if n >= 250 {
                    insert(
                        repo,
                        &mut index,
                        &format!("benchmark_fixture/renamed_{n:04}.txt"),
                        b"retained benchmark fixture\n",
                    )?;
                }
            }
        }
        _ => unreachable!(),
    }
    let oid = index.write_tree_to(repo)?;
    Ok((index, oid))
}
fn changes(repo: &Repository, before: Oid, after: Oid) -> Result<Vec<serde_json::Value>> {
    let mut diff = repo.diff_tree_to_tree(
        Some(&repo.find_tree(before)?),
        Some(&repo.find_tree(after)?),
        None,
    )?;
    let mut options = git2::DiffFindOptions::new();
    options.renames(true).rename_limit(1000);
    diff.find_similar(Some(&mut options))?;
    ensure!(
        diff.deltas().len() <= 1000,
        "benchmark delta inventory exceeds 1000"
    );
    diff.deltas().map(|delta| {
        let fact = |file: git2::DiffFile<'_>| -> Result<serde_json::Value> {
            let raw = if file.id().is_zero() { None } else {
                let blob = repo.find_blob(file.id())?;
                ensure!(blob.size() <= 64*1024*1024, "benchmark change exceeds blob limit");
                Some(digest(blob.content()))
            };
            Ok(serde_json::json!({"path":file.path(),"oid":file.id().to_string(),"sha256":raw}))
        };
        Ok(serde_json::json!({"status":format!("{:?}",delta.status()),"before":fact(delta.old_file())?,"after":fact(delta.new_file())?}))
    }).collect()
}
pub(crate) fn prepare(
    source: &Path,
    destination: &Path,
    selected: &[String],
    kind: Kind,
) -> Result<Manifest> {
    let selected = names(selected)?;
    let source = Source::open(source)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    ensure!(
        !parent.starts_with(source.root()) && !parent.starts_with(source.common_dir()),
        "benchmark fixture must be outside the source checkout and Git directory"
    );
    ensure!(
        matches!(kind, Kind::Commit | Kind::Index),
        "benchmark preparation supports commit HEAD or index only"
    );
    let head = source.resolve(Kind::Commit, "HEAD", None)?;
    let captured = source.resolve(kind, "HEAD", None)?;
    let parent = Oid::from_str(
        head.commit_oid
            .as_deref()
            .context("source HEAD must be a commit")?,
    )?;
    let tree = Oid::from_str(&captured.tree_oid)?;
    let snapshot = Snapshot::create(&source, &captured.tree_oid, destination, Limits::default())?;
    ensure!(
        snapshot.report().skipped_gitlinks == 0,
        "benchmark refuses gitlinks"
    );
    let root = snapshot.path();
    let repository = Repository::init(&root)?;
    import(&repository, source.repository(), &[parent, tree])?;
    let config = conductor_native::project_config::load(&root).map_err(anyhow::Error::msg)?;
    let package = conductor_native::project_paths::package_root_from_config(Some(&config))
        .map_err(anyhow::Error::msg)?;
    let (commit, baseline_tree) = baseline(&repository, tree, parent, &package, &selected)?;
    let admitted = Source::open(&root)?;
    let unused = root.join(crate::resource_id("benchmark-admission")?);
    crate::candidate_snapshot::preflight(
        &admitted,
        &baseline_tree.to_string(),
        &unused,
        Limits::default(),
    )?;
    let mut scenarios = Vec::new();
    for name in selected {
        let path = index_path(&root, &name);
        let (mut index, after) =
            scenario_index(&repository, baseline_tree, &package, &name, Some(&path))?;
        crate::candidate_snapshot::preflight(
            &admitted,
            &after.to_string(),
            &unused,
            Limits::default(),
        )?;
        index.write()?;
        let raw = crate::bounded_file::read(&path, 64 * 1024 * 1024)?;
        scenarios.push(Scenario {
            profile: if name == "full-review" {
                "full".into()
            } else {
                "fast".into()
            },
            name,
            tree_oid: after.to_string(),
            index_sha256: digest(&raw),
            changes: changes(&repository, baseline_tree, after)?,
        });
    }
    let manifest = Manifest {
        schema_version: 1,
        workload_revision: data::WORKLOAD_REVISION.into(),
        source_root: source.root().into(),
        source_kind: kind,
        source_head: parent.to_string(),
        source_tree: tree.to_string(),
        baseline_commit: commit.to_string(),
        baseline_tree: baseline_tree.to_string(),
        package_root: package,
        scenarios,
    };
    crate::candidate_report_store::publish(
        &root.join(MANIFEST),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    snapshot.publish()?;
    Ok(manifest)
}
pub(crate) fn load(root: &Path) -> Result<Manifest> {
    let source = Source::open(root)?;
    ensure!(
        source.root() == root.canonicalize()?
            && source.common_dir() == root.join(".git").canonicalize()?,
        "benchmark requires its standalone data repository"
    );
    let raw = crate::bounded_file::read(&root.join(MANIFEST), 1024 * 1024)?;
    let manifest: Manifest = serde_json::from_value(
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&raw)?, 1024 * 1024)?,
    )?;
    ensure!(
        manifest.schema_version == 1 && manifest.workload_revision == data::WORKLOAD_REVISION,
        "unsupported benchmark fixture revision"
    );
    conductor_native::project_paths::relative(&manifest.package_root, "benchmark package_root")
        .map_err(anyhow::Error::msg)?;
    ensure!(
        matches!(manifest.source_kind, Kind::Commit | Kind::Index),
        "unsupported captured source kind"
    );
    if manifest.source_kind == Kind::Commit {
        ensure!(
            repo_tree(&source, &manifest.source_head)? == manifest.source_tree,
            "captured source tree changed"
        );
    }
    let selected = names(
        &manifest
            .scenarios
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
    )?;
    ensure!(
        !manifest.scenarios.is_empty(),
        "benchmark inventory is empty"
    );
    ensure!(
        std::fs::symlink_metadata(root.join(".git"))?
            .file_type()
            .is_dir(),
        "benchmark Git directory must not be a symlink or indirection file"
    );
    let repo = source.repository();
    admit_generated_paths(
        repo,
        Oid::from_str(&manifest.source_tree)?,
        &manifest.package_root,
        &selected,
    )?;
    let baseline = repo.find_commit(Oid::from_str(&manifest.baseline_commit)?)?;
    ensure!(
        baseline.tree_id().to_string() == manifest.baseline_tree
            && baseline.parent_count() == 1
            && baseline.parent_id(0)?.to_string() == manifest.source_head,
        "benchmark baseline identity changed"
    );
    ensure!(
        repo.head()?.target() == Some(baseline.id()),
        "benchmark HEAD changed"
    );
    for scenario in &manifest.scenarios {
        ensure!(
            scenario.profile
                == if scenario.name == "full-review" {
                    "full"
                } else {
                    "fast"
                },
            "benchmark profile changed"
        );
        let raw = crate::bounded_file::read(&index_path(root, &scenario.name), 64 * 1024 * 1024)?;
        ensure!(
            digest(&raw) == scenario.index_sha256,
            "benchmark index changed"
        );
        let mut admitted = crate::candidate_source::parse_index(&raw)?;
        ensure!(
            admitted.write_tree_to(repo)?.to_string() == scenario.tree_oid,
            "benchmark index/tree mismatch"
        );
        let (_, expected) = scenario_index(
            repo,
            baseline.tree_id(),
            &manifest.package_root,
            &scenario.name,
            None,
        )?;
        ensure!(
            expected.to_string() == scenario.tree_oid
                && changes(repo, baseline.tree_id(), expected)? == scenario.changes,
            "benchmark scenario inputs changed"
        );
    }
    Ok(manifest)
}

fn repo_tree(source: &Source, commit: &str) -> Result<String> {
    source.commit_tree(commit)
}
