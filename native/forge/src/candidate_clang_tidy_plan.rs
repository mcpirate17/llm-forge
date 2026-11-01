//! Plan every selected translation-unit configuration before launching a tool.
use super::{
    config,
    context::{Artifact, Context},
};
use crate::candidate_source::lines::{RangePlan, RangeScope};
use anyhow::{ensure, Context as _, Result};
use conductor_native::candidate_clang_args::{self as args, InputKind, MappingKind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) struct Invocation {
    pub file: String,
    pub database: Vec<u8>,
    pub config: Arc<Vec<u8>>,
    pub record: usize,
    pub directory: PathBuf,
    pub headers: Arc<Vec<PathBuf>>,
}
pub(super) struct Plan {
    pub invocations: Vec<Invocation>,
    pub line_filter: String,
}

fn relative(path: &Path, root: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .context("compiler path escaped snapshot")?
        .to_str()
        .context("compiler path must be UTF-8")?
        .into())
}

fn indexed(context: &Context) -> Result<BTreeMap<String, Vec<usize>>> {
    let mut result: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut bytes = 0usize;
    for (index, record) in context.database.entries.iter().enumerate() {
        let path =
            args::source_path(&record.directory, &record.file).map_err(anyhow::Error::msg)?;
        let Ok(path) = path.strip_prefix(&context.source_root) else {
            continue;
        };
        let path = path.to_str().context("database path must be UTF-8")?;
        crate::candidate_source::safe_path(path.as_bytes())?;
        bytes = bytes
            .checked_add(path.len())
            .context("database index size overflow")?;
        ensure!(
            bytes <= 16 * 1024 * 1024,
            "database index paths exceed 16 MiB"
        );
        result.entry(path.into()).or_default().push(index);
    }
    Ok(result)
}

fn units(
    context: &Context,
    ranges: &RangePlan,
    index: &BTreeMap<String, Vec<usize>>,
) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    let mut units: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for file in &ranges.files {
        if file.scope != RangeScope::Changed {
            continue;
        }
        let extension = Path::new(&file.path)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        ensure!(
            matches!(
                extension.as_str(),
                "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx"
            ),
            "clang-tidy selection must be host C/C++ source or header"
        );
        if let Some(mapped) = context.header_units.get(&file.path) {
            ensure!(
                matches!(extension.as_str(), "h" | "hh" | "hpp" | "hxx"),
                "header mapping applied to non-header source"
            );
            for unit in mapped {
                ensure!(
                    index.contains_key(unit),
                    "mapped header translation unit has no compilation database entry"
                );
                units
                    .entry(unit.clone())
                    .or_default()
                    .push(context.root().join(&file.path));
            }
        } else {
            ensure!(
                index.contains_key(&file.path),
                "selected source/header has no compilation database entry or header mapping"
            );
            units.entry(file.path.clone()).or_default();
        }
        ensure!(units.len() <= 4096, "too many selected translation units");
    }
    Ok(units)
}

fn capture_inputs(context: &mut Context, command: &args::Command) -> Result<()> {
    for input in &command.inputs {
        match (input.mapping, input.kind) {
            (MappingKind::Source, InputKind::Directory) => context.directory(&input.path, false)?,
            (MappingKind::Source, _) => {
                context.source_file(&relative(&input.path, context.root())?)?
            }
            (MappingKind::External, InputKind::Directory) => {
                context.external_directory(&input.path)?
            }
            (MappingKind::External, InputKind::File) => {
                // Generated forced-include files require a pinned artifact rather
                // than an unchecked path under a granted directory.
                anyhow::bail!("external forced include requires captured source input");
            }
            (MappingKind::External, InputKind::Source) => {
                anyhow::bail!("translation unit is outside captured source")
            }
        }
    }
    Ok(())
}

fn config_ref(context: &Context, file: &str) -> Option<Artifact> {
    if let Some(config) = &context.config {
        return Some(config.clone());
    }
    let mut current = Path::new(file).parent();
    while let Some(directory) = current {
        let path = directory.join(".clang-tidy");
        let path = path.to_str().expect("UTF-8 source ancestors");
        if context.tree.contains_key(path) {
            return Some(Artifact::Snapshot { path: path.into() });
        }
        current = directory.parent();
    }
    None
}

fn style(
    context: &mut Context,
    file: &str,
    cache: &mut BTreeMap<String, Arc<Vec<u8>>>,
    total: &mut usize,
) -> Result<Arc<Vec<u8>>> {
    let artifact = config_ref(context, file);
    let key = match &artifact {
        None => "default".into(),
        Some(Artifact::Snapshot { path }) => format!("snapshot:{path}"),
        Some(Artifact::External { path, sha256, .. }) => {
            format!("external:{}:{sha256}", path.display())
        }
    };
    if let Some(value) = cache.get(&key) {
        return Ok(Arc::clone(value));
    }
    let bytes = if let Some(artifact) = artifact {
        context.artifact(&artifact, 64 * 1024)?
    } else {
        b"{}".to_vec()
    };
    let bytes = config::sanitize(&bytes)?;
    *total = total
        .checked_add(bytes.len())
        .context("tidy config byte overflow")?;
    ensure!(*total <= 1024 * 1024, "tidy configurations exceed 1 MiB");
    let value = Arc::new(bytes);
    cache.insert(key, Arc::clone(&value));
    Ok(value)
}

impl Plan {
    pub fn new(context: &mut Context, ranges: &RangePlan) -> Result<Self> {
        let index = indexed(context)?;
        let selected = units(context, ranges, &index)?;
        let mut line_filters = Vec::new();
        for file in &ranges.files {
            if file.scope != RangeScope::Changed {
                continue;
            }
            context.source_file(&file.path)?;
            line_filters.push(json!({"name":context.root().join(&file.path),
                "lines":file.ranges.iter().map(|r| [r.start,r.end]).collect::<Vec<_>>()}));
        }
        let line_filter = serde_json::to_string(&line_filters)?;
        ensure!(
            line_filter.len() <= 64 * 1024,
            "clang-tidy line filter exceeds 64 KiB"
        );
        let mut result = Self {
            invocations: Vec::new(),
            line_filter,
        };
        let mut cache = BTreeMap::new();
        let (mut config_bytes, mut output_bytes) = (0usize, 0usize);
        for (file, headers) in selected {
            let headers = Arc::new(headers);
            let config = style(context, &file, &mut cache, &mut config_bytes)?;
            for record_index in &index[&file] {
                ensure!(
                    result.invocations.len() < 4096,
                    "selected build configurations exceed 4096"
                );
                let record = &context.database.entries[*record_index];
                let driver =
                    args::virtual_driver(&record.arguments[0]).map_err(anyhow::Error::msg)?;
                let mut command = args::rebase(
                    &record.directory,
                    &record.file,
                    &record.arguments,
                    &context.mappings,
                    driver,
                )
                .map_err(anyhow::Error::msg)?;
                capture_inputs(context, &command)?;
                command.arguments.push(format!(
                    "-resource-dir={}",
                    context
                        .resource_dir
                        .to_str()
                        .context("resource path must be UTF-8")?
                ));
                if !headers.is_empty() {
                    command.arguments.push("-H".into());
                }
                let database = serde_json::to_vec(
                    &json!([{"directory":command.directory,"file":command.file,"arguments":command.arguments}]),
                )?;
                output_bytes = output_bytes
                    .checked_add(database.len())
                    .and_then(|v| v.checked_add(config.len()))
                    .context("tidy plan byte overflow")?;
                ensure!(
                    output_bytes <= 64 * 1024 * 1024,
                    "normalized tidy inputs exceed 64 MiB"
                );
                result.invocations.push(Invocation {
                    file: file.clone(),
                    record: *record_index,
                    directory: command.directory,
                    headers: Arc::clone(&headers),
                    database,
                    config: Arc::clone(&config),
                });
            }
        }
        context.verify()?;
        Ok(result)
    }

    pub fn report(&self) -> Value {
        json!({"configurations":self.invocations.iter().map(|entry| json!({"file":entry.file,"database_record":entry.record,
            "required_headers":entry.headers.as_ref(), "coverage":"required at execution",
            "database_sha256":format!("{:x}",Sha256::digest(&entry.database)),"config_sha256":format!("{:x}",Sha256::digest(entry.config.as_slice()))})).collect::<Vec<_>>(),
            "line_filter":self.line_filter,"invocation_count":self.invocations.len()})
    }
}
