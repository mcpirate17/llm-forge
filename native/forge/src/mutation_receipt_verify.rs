//! Four target-tree byte-binding checks. Does not certify mutation outcomes.
#[path = "mutation_receipt_worker.rs"]
mod worker;
use crate::{bounded_file, candidate_source::Source, tool_schema};
use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::receipt_auth::{self, SourceRead, TargetTreeReceipt};
use git2::{ObjectType, Oid};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};
const RECEIPT_LIMIT: usize = 32 * 1024 * 1024;
const BLOB_LIMIT: usize = 32 * 1024 * 1024;
const TOTAL_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Args)]
pub struct VerifyArgs {
    /// Mutation receipt JSON containing its manifest and source pin inventory.
    #[arg(long)]
    receipt: PathBuf,
    /// Commit, tag or tree naming the immutable target tree to measure.
    #[arg(long)]
    tree: String,
    /// Working repository; omit to discover from the current directory.
    #[arg(long)]
    repo: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    /// Internal anonymous result channel; direct entry also installs kernel limits.
    #[arg(long, hide = true)]
    worker_fd: Option<i32>,
}
pub fn run(args: VerifyArgs) -> u8 {
    if let Some(fd) = args.worker_fd {
        return match worker::execute(&args, fd) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("REFUSED: {error:#}");
                4
            }
        };
    }
    match worker::supervise(&args) {
        Ok(verdict) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&verdict).expect("bounded verdict serializes")
                );
            } else {
                human(&verdict);
            }
            if verdict["status"] == "PASS" {
                0
            } else {
                1
            }
        }
        Err(error) => {
            if args.json {
                println!(
                    "{}",
                    json!({"status":"REFUSED","error":format!("{error:#}")})
                );
            } else {
                eprintln!("REFUSED: {error:#}");
            }
            4
        }
    }
}
fn measure(args: &VerifyArgs) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    let source = Source::open(args.repo.as_deref().unwrap_or(&cwd))?;
    let label = args.tree.trim();
    ensure!(
        !label.is_empty()
            && label.len() <= 4096
            && !label.contains('\0')
            && !label.starts_with('-'),
        "invalid --tree reference"
    );
    // Revision parsing may allocate intermediate objects; the worker's kernel
    // ceiling contains that work. Semantic admission is not a peak-memory claim.
    let tree = resolve_tree(&source, label)?;
    let root = source
        .root()
        .to_str()
        .context("repository root must be UTF-8")?;
    let oid = tree.to_string();
    // Resolution precedes receipt reading; JSON stdout stays exactly one object.
    if args.json {
        eprintln!("repo_root={root}\ntree_oid={oid}");
    } else {
        println!("repo_root={root}\ntree_oid={oid}");
    }
    let receipt = load(&args.receipt)?;
    let mut target = Target {
        source: &source,
        tree,
        bytes: 0,
        trees: BTreeSet::new(),
        tree_bytes: 0,
        entries: 0,
    };
    let manifest = target.blob(receipt.manifest(), 2 * 1024 * 1024)?;
    let mut reads = BTreeMap::new();
    if let Some(bytes) = &manifest.bytes {
        tool_schema::parse_document_with_limit(std::str::from_utf8(bytes)?, 2 * 1024 * 1024)?;
        let pins =
            receipt_auth::manifest_pins(bytes, receipt.manifest()).map_err(anyhow::Error::msg)?;
        for path in pins.keys() {
            let read = target.blob(path, BLOB_LIMIT)?;
            reads.insert(
                path.clone(),
                SourceRead {
                    sha256: read.bytes.as_deref().map(hash),
                    reason: read.reason,
                },
            );
        }
    }
    let verdict = receipt_auth::verify(
        receipt,
        root,
        &oid,
        manifest.bytes.as_deref(),
        &manifest.reason,
        &reads,
    )
    .map_err(anyhow::Error::msg)?;
    crate::json_canon::encoded_size(&verdict, 16 * 1024 * 1024)?;
    ensure!(
        serde_json::to_vec_pretty(&verdict)?.len() <= 16 * 1024 * 1024,
        "receipt verdict output exceeds 16 MiB"
    );
    Ok(verdict)
}
fn metadata_limit(kind: ObjectType) -> Result<usize> {
    match kind {
        ObjectType::Tree => Ok(8 * 1024 * 1024),
        ObjectType::Commit | ObjectType::Tag => Ok(1024 * 1024),
        _ => anyhow::bail!("target reference does not name a treeish"),
    }
}
fn admit_metadata(source: &Source, id: Oid, expected: Option<ObjectType>) -> Result<()> {
    let (size, kind) = source.repository().odb()?.read_header(id)?;
    ensure!(
        expected.is_none_or(|expected| expected == kind),
        "target metadata type differs"
    );
    let limit = metadata_limit(kind)?;
    ensure!(
        size <= limit,
        "target {} metadata exceeds {} MiB limit",
        kind.str(),
        limit / (1024 * 1024)
    );
    Ok(())
}
fn resolve_tree(source: &Source, label: &str) -> Result<Oid> {
    let repo = source.repository();
    let mut object = repo
        .revparse_single(label)
        .with_context(|| format!("cannot resolve {label:?} to a tree"))?;
    let mut tag_hops = 0;
    loop {
        admit_metadata(source, object.id(), object.kind())?;
        match object.kind() {
            Some(ObjectType::Tree) => return Ok(object.id()),
            Some(ObjectType::Commit) => {
                let tree = object.as_commit().context("expected commit")?.tree_id();
                admit_metadata(source, tree, Some(ObjectType::Tree))?;
                return Ok(repo.find_tree(tree)?.id());
            }
            Some(ObjectType::Tag) => {
                ensure!(tag_hops < 32, "target tag peeling exceeds 32 hops");
                let tag = object.as_tag().context("expected tag")?;
                let (next, kind) = (tag.target_id(), tag.target_type());
                admit_metadata(source, next, kind)?;
                object = repo.find_object(next, kind)?;
                tag_hops += 1;
            }
            _ => anyhow::bail!("target reference does not name a treeish"),
        }
    }
}

fn load(path: &Path) -> Result<TargetTreeReceipt> {
    let label = path.to_str().context("receipt path must be UTF-8")?;
    ensure!(label.len() <= 4096, "receipt path exceeds4096 bytes");
    let raw = bounded_file::read(path, RECEIPT_LIMIT)
        .with_context(|| format!("cannot read receipt file {label}"))?;
    ensure!(!raw.is_empty(), "receipt file is empty: {label}");
    let payload = tool_schema::parse_document_with_limit(std::str::from_utf8(&raw)?, RECEIPT_LIMIT)
        .with_context(|| format!("receipt is not valid JSON: {label}"))?;
    receipt_auth::parse_receipt_value(label, &payload).map_err(anyhow::Error::msg)
}
struct Read {
    bytes: Option<Vec<u8>>,
    reason: String,
}
struct Target<'a> {
    source: &'a Source,
    tree: Oid,
    bytes: usize,
    trees: BTreeSet<Oid>,
    tree_bytes: usize,
    entries: usize,
}
impl Target<'_> {
    fn entry(&mut self, path: &str) -> Result<Option<(Oid, Option<ObjectType>)>> {
        let mut id = self.tree;
        let mut parts = path.split('/').peekable();
        while let Some(name) = parts.next() {
            let odb = self.source.repository().odb()?;
            let (size, kind) = odb.read_header(id)?;
            ensure!(
                kind == ObjectType::Tree && size <= 8 * 1024 * 1024,
                "target tree metadata exceeds limit or has wrong type"
            );
            let fresh = !self.trees.contains(&id);
            if fresh {
                let next = self
                    .tree_bytes
                    .checked_add(size)
                    .context("target tree byte overflow")?;
                ensure!(
                    self.trees.len() < 4096 && next <= 64 * 1024 * 1024,
                    "target tree metadata inventory exceeds limits"
                );
                self.tree_bytes = next;
            }
            let tree = self.source.repository().find_tree(id)?;
            if self.trees.insert(id) {
                self.entries = self
                    .entries
                    .checked_add(tree.len())
                    .context("target tree entry overflow")?;
                ensure!(
                    self.trees.len() <= 4096
                        && self.tree_bytes <= 64 * 1024 * 1024
                        && self.entries <= 100_000,
                    "target tree metadata inventory exceeds limits"
                );
            }
            let Some(entry) = tree.get_name(name) else {
                return Ok(None);
            };
            id = entry.id();
            let kind = entry.kind();
            if parts.peek().is_none() {
                return Ok(Some((id, kind)));
            }
            if kind != Some(ObjectType::Tree) {
                return Ok(None);
            }
        }
        Ok(None)
    }

    fn blob(&mut self, path: &str, maximum: usize) -> Result<Read> {
        let Some((id, kind)) = self.entry(path)? else {
            return Ok(unavailable("path is absent from target tree"));
        };
        if kind != Some(ObjectType::Blob) {
            return Ok(unavailable("path is not a blob in target tree"));
        }
        // Mode120000 is still a Git blob. Preserve cat-file semantics rather than
        // interpreting its contents as a live filesystem symlink.
        let odb = self.source.repository().odb()?;
        let (size, kind) = match odb.read_header(id) {
            Ok(header) => header,
            Err(error) => {
                return Ok(unavailable(&format!(
                    "cannot read target-tree blob header: {error}"
                )))
            }
        };
        ensure!(
            kind == ObjectType::Blob && size <= maximum,
            "target-tree blob exceeds byte limit or has wrong type: {path}"
        );
        self.bytes = self
            .bytes
            .checked_add(size)
            .context("target-tree byte count overflow")?;
        ensure!(
            self.bytes <= TOTAL_LIMIT,
            "target-tree blob reads exceed64MiB"
        );
        let blob = match self.source.repository().find_blob(id) {
            Ok(blob) => blob,
            Err(error) => {
                return Ok(unavailable(&format!(
                    "cannot read target-tree blob: {error}"
                )))
            }
        };
        ensure!(
            blob.size() == size && Oid::hash_object(ObjectType::Blob, blob.content())? == id,
            "target-tree blob identity differs: {path}"
        );
        Ok(Read {
            bytes: Some(blob.content().to_vec()),
            reason: String::new(),
        })
    }
}
fn unavailable(reason: &str) -> Read {
    let mut chars = reason.chars();
    let mut reason: String = chars.by_ref().take(1000).collect();
    if chars.next().is_some() {
        reason.push_str(" [truncated]");
    }
    Read {
        bytes: None,
        reason,
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn human(verdict: &Value) {
    if verdict["status"] == "PASS" {
        let part = &verdict["checks"]["inventory_digest"];
        println!(
            "PASS receipt={} manifest={} tree={} pins={} inventory_digest={}",
            verdict["receipt"].as_str().expect("typed verdict"),
            verdict["manifest"].as_str().expect("typed verdict"),
            verdict["tree_oid"].as_str().expect("typed verdict"),
            part["pins"],
            part["digest"].as_str().expect("typed verdict")
        );
    } else {
        let failures = verdict["failures"].as_array().expect("typed verdict");
        for line in failures {
            println!("FAIL {}", line.as_str().expect("typed failure"));
        }
        println!(
            "FAIL {} failure(s) against tree {}",
            failures.len(),
            verdict["tree_oid"].as_str().expect("typed verdict")
        );
    }
}
/// Preserve the old module's pasted smart-dash repair only for this leaf route.
pub fn normalize_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args: Vec<_> = args.into_iter().collect();
    if args.get(1).is_some_and(|s| s == "receipt") && args.get(2).is_some_and(|s| s == "verify") {
        for arg in args.iter_mut().skip(3) {
            if let Some(text) = arg.to_str() {
                if text.starts_with('—') || text.starts_with('–') {
                    *arg = OsString::from(format!(
                        "--{}",
                        &text[text
                            .chars()
                            .next()
                            .expect("nonempty smart-dash arg")
                            .len_utf8()..]
                    ));
                }
            }
        }
    }
    args
}
