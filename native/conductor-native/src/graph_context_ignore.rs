//! Bounded repo-authored ignore rules; no global Git config or ancestor rules.
//! `.rgignore` outranks `.ignore`, which outranks `.gitignore` regardless of
//! depth; within each type the closest directory wins. Ignored directories
//! are pruned, so children cannot re-include themselves through an ignored parent.
use crate::graph_context::files::SourceRoot;
use anyhow::{ensure, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Path;

const MAX_IGNORE_FILE: usize = 64 * 1024;
const MAX_IGNORE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RULES: usize = 20_000;
const MAX_PROBES: usize = 12_288;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const NAMES: [&str; 3] = [".gitignore", ".ignore", ".rgignore"];
struct Layer {
    rules: [Option<Gitignore>; 3],
}
struct Evidence {
    path: String,
    sha256: Option<String>,
}
pub(super) struct IgnoreState {
    layers: Vec<Layer>,
    evidence: Vec<Evidence>,
    bytes: usize,
    rules: usize,
    path_bytes: usize,
}
impl IgnoreState {
    pub(super) fn new() -> Self {
        Self {
            layers: Vec::new(),
            evidence: Vec::new(),
            bytes: 0,
            rules: 0,
            path_bytes: 0,
        }
    }
    pub(super) fn enter(&mut self, sources: &SourceRoot, directory: &Path) -> Result<()> {
        let mut rules = [None, None, None];
        for (kind, name) in NAMES.iter().enumerate() {
            let path = directory.join(name);
            let path = path.to_str().context("ignore path must be UTF-8")?;
            ensure!(
                self.evidence.len() < MAX_PROBES,
                "syntactic ignore probe limit exceeded"
            );
            self.path_bytes += path.len();
            ensure!(
                self.path_bytes <= MAX_PATH_BYTES,
                "syntactic ignore path byte limit exceeded"
            );
            let snapshot = sources.read_text(path, MAX_IGNORE_FILE)?;
            let sha256 = snapshot.as_ref().map(|s| s.sha256.clone());
            if let Some(snapshot) = snapshot {
                self.bytes += snapshot.text.len();
                ensure!(
                    self.bytes <= MAX_IGNORE_BYTES,
                    "syntactic ignore total byte limit exceeded"
                );
                let mut builder = GitignoreBuilder::new(sources.host().join(directory));
                let origin = sources.host().join(path);
                for line in snapshot.text.lines() {
                    self.rules += 1;
                    ensure!(
                        self.rules <= MAX_RULES,
                        "syntactic ignore rule limit exceeded"
                    );
                    builder
                        .add_line(Some(origin.clone()), line)
                        .context("invalid syntactic ignore rule")?;
                }
                rules[kind] = Some(builder.build().context("compile syntactic ignore rules")?);
            }
            self.evidence.push(Evidence {
                path: path.to_owned(),
                sha256,
            });
        }
        self.layers.push(Layer { rules });
        Ok(())
    }
    pub(super) fn leave(&mut self) {
        self.layers.pop();
    }
    pub(super) fn ignored(&self, absolute: &Path, is_dir: bool) -> bool {
        for kind in (0..NAMES.len()).rev() {
            for layer in self.layers.iter().rev() {
                if let Some(matcher) = &layer.rules[kind] {
                    let result = matcher.matched(absolute, is_dir);
                    if !result.is_none() {
                        return result.is_ignore();
                    }
                }
            }
        }
        false
    }
    pub(super) fn verify(
        &self,
        sources: &SourceRoot,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        for evidence in &self.evidence {
            check()?;
            ensure!(
                sources.hash(&evidence.path)? == evidence.sha256,
                "syntactic ignore file appeared, disappeared, or changed: {}",
                evidence.path
            );
        }
        Ok(())
    }
}
