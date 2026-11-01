//! Snapshot top-level cards and publish only after bounded embedding and revalidation.
use super::{
    fingerprint,
    index::{Card, CardIndex, Metadata, Provenance},
    MAX_CARDS, MAX_INDEX_BYTES, NATIVE_RECIPE,
};
use crate::{embedding, file_lock, source_file::SourceRoot, state_publish};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt as CapMetadataExt, OpenOptions, OpenOptionsExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 65_536;
const MAX_DOCUMENT: usize = 16_384;
pub struct Settings {
    pub host: PathBuf,
    pub notes: Option<PathBuf>,
    pub output: PathBuf,
    pub embedding: embedding::Config,
    pub timeout_seconds: u64,
}
#[derive(Serialize)]
pub struct BuildReport {
    pub schema_version: u32,
    pub output: PathBuf,
    pub cards: usize,
    pub embedding_fingerprint: String,
    pub source_set_sha256: String,
    pub input_tokens: u64,
    pub index_sha256: String,
}
struct Input {
    name: String,
    path: PathBuf,
    text: String,
    hash: String,
}

fn absolute(host: &Path, path: &Path) -> Result<PathBuf> {
    let full = if path.is_absolute() {
        path.to_owned()
    } else {
        host.join(path)
    };
    ensure!(
        full.is_absolute()
            && full.components().all(|c| matches!(
                c,
                Component::RootDir | Component::Normal(_) | Component::CurDir
            )),
        "path must be absolute without traversal"
    );
    Ok(full.components().collect())
}
fn open_directory(path: &Path) -> Result<Dir> {
    ensure!(path.is_absolute(), "directory must be absolute");
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => {
                let file = dir
                    .open_with(
                        name,
                        OpenOptions::new().read(true).custom_flags(
                            libc::O_DIRECTORY
                                | libc::O_NOFOLLOW
                                | libc::O_NONBLOCK
                                | libc::O_CLOEXEC,
                        ),
                    )
                    .with_context(|| format!("opening directory {}", path.display()))?;
                dir = Dir::from_std_file(file.into_std());
            }
            _ => anyhow::bail!("directory contains traversal"),
        }
    }
    Ok(dir)
}
pub(super) fn card_name(raw: &[u8]) -> bool {
    (raw.starts_with(b"kb_") || raw.starts_with(b"KB-")) && raw.ends_with(b".md")
}
fn names(dir: &Dir) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for (count, entry) in dir.entries()?.enumerate() {
        ensure!(count < MAX_ENTRIES, "notes inventory exceeds 65536 entries");
        let entry = entry?;
        let name = entry.file_name();
        if !card_name(name.as_bytes()) {
            continue;
        }
        let name = name.to_str().context("card name must be UTF-8")?;
        ensure!(
            name.len() <= 4096 && names.len() < MAX_CARDS,
            "card count or filename exceeds bound"
        );
        names.push(name.to_owned());
    }
    names.sort();
    ensure!(
        !names.is_empty(),
        "no knowledge cards found in notes directory"
    );
    Ok(names)
}
fn snapshot(notes: &Path, dir: &Dir, selected: &[String]) -> Result<Vec<Input>> {
    let source = SourceRoot::open(&PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())))?;
    selected
        .iter()
        .map(|name| {
            let item = source.read(Path::new(name))?;
            ensure!(
                !item.content().is_empty() && item.content().len() <= MAX_DOCUMENT,
                "card {name:?} must contain 1..16384 bytes for native embedding"
            );
            let full_path = notes.join(name);
            ensure!(
                full_path.to_str().is_some_and(|path| path.len() <= 4096),
                "card path must be UTF-8 and at most 4096 bytes"
            );
            Ok(Input {
                name: name.clone(),
                path: full_path,
                text: item.content().to_owned(),
                hash: item.sha256().to_owned(),
            })
        })
        .collect()
}
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
fn target_stamp(path: &Path) -> Result<Option<Stamp>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(meta.is_file() && meta.uid()==unsafe{libc::geteuid()} && meta.nlink()==1 && meta.mode() & 0o022 == 0,
                "card output must be an owned regular file without hard links or other-user write access");
            Ok(Some(Stamp {
                dev: meta.dev(),
                ino: meta.ino(),
                len: meta.len(),
                mtime: meta.mtime(),
                mtime_ns: meta.mtime_nsec(),
                ctime: meta.ctime(),
                ctime_ns: meta.ctime_nsec(),
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn limit(inputs: &[Input], dims: usize) -> Result<()> {
    let text = inputs
        .iter()
        .try_fold(0usize, |total, item| {
            let strings = item
                .text
                .len()
                .checked_add(item.name.len())?
                .checked_add(item.path.as_os_str().as_bytes().len())?;
            total.checked_add(strings.checked_mul(6)?)?.checked_add(512)
        })
        .context("card size overflow")?;
    let vectors = inputs
        .len()
        .checked_mul(dims)
        .and_then(|n| n.checked_mul(26))
        .context("card vector size overflow")?;
    ensure!(
        text.checked_add(vectors)
            .is_some_and(|n| n <= MAX_INDEX_BYTES),
        "card index worst-case encoded size exceeds 16 MiB"
    );
    Ok(())
}
fn embed(
    inputs: &[Input],
    config: &embedding::Config,
    deadline: Instant,
) -> Result<(Vec<Card>, u64)> {
    let client = embedding::Client::new(config.clone())?;
    let mut cards = Vec::with_capacity(inputs.len());
    let mut total = 0_u64;
    let mut start = 0;
    while start < inputs.len() {
        ensure!(Instant::now() < deadline, "card build deadline expired");
        let mut end = start;
        let mut bytes = 0;
        while end < inputs.len() && end - start < 32 && bytes + inputs[end].text.len() <= 65536 {
            bytes += inputs[end].text.len();
            end += 1;
        }
        ensure!(end > start, "card text exceeds embedding batch bound");
        let texts: Vec<_> = inputs[start..end]
            .iter()
            .map(|item| item.text.as_str())
            .collect();
        let batch = client.embed_batch(&texts, deadline)?;
        total = total
            .checked_add(batch.input_tokens_total)
            .context("card token total overflow")?;
        for (item, vector) in inputs[start..end].iter().zip(batch.vectors) {
            cards.push(Card {
                name: item.name.clone(),
                path: item
                    .path
                    .to_str()
                    .context("card path must be UTF-8")?
                    .into(),
                text: item.text.clone(),
                vector: vector.into_iter().map(f64::from).collect(),
                source_sha256: Some(item.hash.clone()),
            });
        }
        start = end;
    }
    Ok((cards, total))
}
fn source_digest(inputs: &[Input]) -> Result<String> {
    let rows: Vec<_> = inputs.iter().map(|i| (&i.name, &i.hash)).collect();
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&rows)?)))
}
fn assemble(
    config: &embedding::Config,
    fingerprint: &str,
    notes: &Path,
    source_set_sha256: &str,
    cards: Vec<Card>,
) -> Result<CardIndex> {
    let index = CardIndex {
        schema_version: 2,
        embedding: Metadata {
            fingerprint: fingerprint.to_owned(),
            dimension: config.dimension,
            paid: false,
            num_gpu: Some(config.gpu_layers as u32),
            num_ctx: Some(config.context_tokens as u32),
            native_recipe: Some(NATIVE_RECIPE.into()),
            artifact_sha256: Some(
                config
                    .artifact_sha256
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ),
            model: Some(config.model.clone()),
            runner: Some(config.runner.clone()),
            threads: Some(config.threads),
            query_prefix_sha256: Some(format!("{:x}", Sha256::digest(super::QUERY_PREFIX))),
        },
        cards,
        provenance: Some(Provenance {
            notes_root: notes
                .to_str()
                .context("notes root must be UTF-8")?
                .to_owned(),
            source_set_sha256: source_set_sha256.to_owned(),
        }),
    };
    Ok(index)
}
// As with state_publish, the operator-controlled parent and its ancestors are trusted
// not to be replaced between the identity check and path-based publication.
fn publish_index(
    config: &embedding::Config,
    fingerprint: &str,
    notes: &Path,
    source_set_sha256: &str,
    cards: Vec<Card>,
    output: &Path,
) -> Result<(usize, String)> {
    let index = assemble(config, fingerprint, notes, source_set_sha256, cards)?;
    index.validate()?;
    let bytes = serde_json::to_vec(&index)?;
    ensure!(bytes.len() <= MAX_INDEX_BYTES, "card index exceeds 16 MiB");
    let index_sha256 = format!("{:x}", Sha256::digest(&bytes));
    state_publish::publish(output, &bytes)?;
    Ok((index.cards.len(), index_sha256))
}
pub fn build(settings: Settings) -> Result<BuildReport> {
    ensure!(
        (1..=3600).contains(&settings.timeout_seconds),
        "card build timeout must be 1..3600 seconds"
    );
    let deadline = Instant::now() + Duration::from_secs(settings.timeout_seconds);
    let host = settings.host.canonicalize().context("card host")?;
    let notes = match &settings.notes {
        Some(path) => absolute(&host, path)?,
        None => absolute(
            &host,
            &conductor_native::project_paths::notes_root(&host).map_err(anyhow::Error::msg)?,
        )?,
    };
    let output = absolute(&host, &settings.output)?;
    let parent = output.parent().context("card output parent")?;
    let output_directory = open_directory(parent)?;
    let parent_meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        parent_meta.is_dir()
            && parent_meta.uid() == unsafe { libc::geteuid() }
            && parent_meta.mode() & 0o022 == 0,
        "card output directory must be owned and not writable by others"
    );
    ensure!(
        !(parent == notes
            && output
                .file_name()
                .is_some_and(|name| card_name(name.as_bytes()))),
        "card output cannot alias a source card"
    );
    let mut lock_name = output.as_os_str().to_os_string();
    lock_name.push(".lock");
    let _lease = file_lock::owned(Path::new(&lock_name), true)?;
    let before = target_stamp(&output)?;
    let fingerprint = fingerprint(&settings.embedding)?;
    let notes_dir = open_directory(&notes)?;
    let selected = names(&notes_dir)?;
    let inputs = snapshot(&notes, &notes_dir, &selected)?;
    limit(&inputs, settings.embedding.dimension)?;
    let source_set_sha256 = source_digest(&inputs)?;
    let (cards, input_tokens) = embed(&inputs, &settings.embedding, deadline)?;
    ensure!(
        Instant::now() < deadline,
        "card build deadline expired before publication"
    );
    ensure!(
        names(&notes_dir)? == selected,
        "card inventory changed before publication"
    );
    let reread = snapshot(&notes, &notes_dir, &selected)?;
    ensure!(
        inputs.iter().zip(&reread).all(|(a, b)| a.hash == b.hash),
        "card source changed before publication"
    );
    for (path, held) in [(notes.as_path(), &notes_dir), (parent, &output_directory)] {
        let old = held.dir_metadata()?;
        let current = open_directory(path)?.dir_metadata()?;
        ensure!(
            (old.dev(), old.ino()) == (current.dev(), current.ino()),
            "card directory changed before publication"
        );
    }
    ensure!(
        target_stamp(&output)? == before,
        "card output changed during build"
    );
    let (card_count, index_sha256) = publish_index(
        &settings.embedding,
        &fingerprint,
        &notes,
        &source_set_sha256,
        cards,
        &output,
    )?;
    Ok(BuildReport {
        schema_version: 1,
        output,
        cards: card_count,
        embedding_fingerprint: fingerprint,
        source_set_sha256,
        input_tokens,
        index_sha256,
    })
}
