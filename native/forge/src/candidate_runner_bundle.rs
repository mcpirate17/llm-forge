//! Observe an explicitly selected installed compatibility package, without
//! executing it. These hashes are pins, not evidence of execution or approval.
#[path = "candidate_runner_bundle_output.rs"]
mod output;
#[cfg(test)]
#[path = "candidate_runner_bundle_tests.rs"]
mod tests;

use crate::{
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    candidate_mutation_runner::{
        FilePin, Manifest, BINDINGS, COMPONENTS, MAX_MANIFEST_BYTES, MAX_PROBE_BYTES,
        MAX_SOURCE_BYTES, MAX_TOTAL_BYTES,
    },
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// `exclude_roots` supplies existing snapshot and scratch directories. The
/// source checkout, its common Git directory, and TMPDIR are always excluded.
/// All input paths and the existing mode-0700 output parent must be canonical.
pub struct Request<'a> {
    pub repo: &'a Path,
    pub package_root: &'a Path,
    pub output: &'a Path,
    pub lineage: Option<&'a Path>,
    pub external_invariant_probe: Option<&'a Path>,
    pub exclude_roots: &'a [PathBuf],
}

pub struct Generated {
    pub manifest_sha256: String,
    pub manifest_bytes: usize,
}

struct Captured<'a> {
    root: DirectoryBinding,
    package_root: &'a Path,
    exclusion: &'a Path,
    held: Vec<BoundFile>,
    bytes: usize,
}

impl<'a> Captured<'a> {
    fn new(package_root: &'a Path, exclusion: &'a Path) -> Result<Self> {
        Ok(Self {
            root: DirectoryBinding::open(package_root, Some(exclusion))?,
            package_root,
            exclusion,
            held: Vec::with_capacity(COMPONENTS.len() + BINDINGS.len() + 2),
            bytes: 0,
        })
    }

    fn admit(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("runner byte overflow")?;
        ensure!(
            self.bytes <= MAX_TOTAL_BYTES,
            "mutation runner bundle exceeds32MiB"
        );
        Ok(())
    }

    fn pin(&mut self, path: &Path, maximum: usize, native: bool) -> Result<FilePin> {
        ensure!(
            path.is_absolute()
                && path.starts_with(self.package_root)
                && path.canonicalize()?.as_os_str() == path.as_os_str(),
            "runner component must be inside canonical installed package root"
        );
        let (held, bytes) = candidate_inputs::observed(path, maximum, self.exclusion)
            .with_context(|| format!("capture installed runner file {}", path.display()))?;
        self.admit(bytes.len())?;
        if native {
            ensure!(
                bytes.starts_with(b"\x7fELF"),
                "external invariant probe must be a native ELF shared library"
            );
        }
        let pin = FilePin {
            path: path.to_path_buf(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        held.verify()?;
        self.held.push(held);
        Ok(pin)
    }

    fn inventory(&mut self, paths: &[&str]) -> Result<BTreeMap<String, FilePin>> {
        paths
            .iter()
            .map(|key| {
                let pin = self.pin(&self.package_root.join(key), MAX_SOURCE_BYTES, false)?;
                Ok(((*key).to_owned(), pin))
            })
            .collect()
    }

    fn verify(&self) -> Result<()> {
        self.root.verify()?;
        for held in &self.held {
            held.verify()?;
        }
        Ok(())
    }
}

fn excluded(path: &Path, roots: &[PathBuf]) -> Result<()> {
    for root in roots {
        ensure!(
            !path.starts_with(root) && !root.starts_with(path),
            "runner bundle input/output overlaps source, Git metadata, snapshot or scratch: {}",
            root.display()
        );
    }
    Ok(())
}

fn exclusions(request: &Request<'_>, source: &Source) -> Result<Vec<PathBuf>> {
    ensure!(
        request.exclude_roots.len() <= 64,
        "runner bundle exceeds64 exclusion roots"
    );
    let mut roots = vec![
        source.root().to_path_buf(),
        source.common_dir().to_path_buf(),
    ];
    for path in request.exclude_roots {
        ensure!(
            path.is_absolute()
                && path.is_dir()
                && path.canonicalize()?.as_os_str() == path.as_os_str(),
            "runner exclusion must be a canonical absolute directory"
        );
        roots.push(path.clone());
    }
    if let Some(path) = std::env::var_os("TMPDIR") {
        let path = PathBuf::from(path);
        ensure!(
            path.is_absolute()
                && path.is_dir()
                && path.canonicalize()?.as_os_str() == path.as_os_str(),
            "TMPDIR must be a canonical absolute directory"
        );
        roots.push(path);
    }
    Ok(roots)
}

/// Hash the exact 11 components and five bindings named by the capture schema.
/// Optional lineage and probe files must also live below `package_root`.
/// Publish complete bytes atomically and fail if the output already exists.
pub fn generate(request: Request<'_>) -> Result<Generated> {
    let source = Source::open(request.repo)?;
    ensure!(
        request.package_root.is_absolute()
            && request.package_root.canonicalize()?.as_os_str() == request.package_root.as_os_str(),
        "runner package root must be canonical and absolute"
    );
    let roots = exclusions(&request, &source)?;
    excluded(request.package_root, &roots)?;
    excluded(request.output, &roots)?;
    ensure!(
        !request.output.starts_with(request.package_root),
        "runner bundle output must be outside installed package root"
    );
    let destination = output::Destination::open(request.output)?;
    let mut captured = Captured::new(request.package_root, source.root())?;
    let payload = Manifest {
        schema_version: 1,
        symbol_hash_protocol: conductor_native::candidate_python_symbol_hash::PROTOCOL.into(),
        package_root: request.package_root.to_path_buf(),
        components: captured.inventory(&COMPONENTS)?,
        bindings: captured.inventory(&BINDINGS)?,
        lineage: request
            .lineage
            .map(|path| captured.pin(path, MAX_SOURCE_BYTES, false))
            .transpose()?,
        external_invariant_probe: request
            .external_invariant_probe
            .map(|path| captured.pin(path, MAX_PROBE_BYTES, true))
            .transpose()?,
    };
    payload.validate()?;
    let mut bytes = serde_json::to_vec_pretty(&payload)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= MAX_MANIFEST_BYTES,
        "runner manifest exceeds1MiB"
    );
    captured.admit(bytes.len())?;
    captured.verify()?;
    destination.publish(&bytes, || captured.verify())?;
    Ok(Generated {
        manifest_sha256: format!("{:x}", Sha256::digest(&bytes)),
        manifest_bytes: bytes.len(),
    })
}
