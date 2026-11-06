//! Reuse held candidate inputs for every registry/config/source/test/receipt byte.
use super::*;
/// Bound repeated held inventories and their file descriptors before opening more.
pub(crate) fn cargo_budget(held: &[crate::mutation_cargo_capture::Held]) -> Result<usize> {
    ensure!(held.len() < 8, "Cargo held inventory count exceeds 8");
    let existing: usize = held.iter().map(|item| item.file_count()).sum();
    256usize
        .checked_sub(existing)
        .filter(|remaining| *remaining > 0)
        .context("Cargo held file budget exceeds 256")
}
pub(crate) struct Capture<'a> {
    pub inputs: Inputs<'a>,
    pub ledger: BTreeMap<String, Value>,
    copied_bytes: usize,
    native_fingerprint: Option<Option<String>>,
    native_inventory: Option<Vec<PathBuf>>,
    cargo_inventories: Vec<crate::mutation_cargo_capture::Held>,
    supporting: Option<crate::mutation_cargo_supporting::Held>,
    workspace: Option<(
        crate::mutation_snapshot::SnapshotSelection,
        conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    )>,
    source_root: PathBuf,
    snapshot: &'a Snapshot,
    pub failed: bool,
}
impl<'a> Capture<'a> {
    /// Capture the absence of every Forge source root, and hold it through verify.
    pub fn native_inventory_absent(&mut self) -> Result<bool> {
        for root in [
            "native/forge",
            "native/forge-cli",
            "native/conductor-native",
            "native/slop-core",
        ] {
            if self
                .inputs
                .entries
                .keys()
                .any(|path| path == root || path.starts_with(&format!("{root}/")))
            {
                return Ok(false);
            }
        }
        for root in [
            "native/forge",
            "native/forge-cli",
            "native/conductor-native",
            "native/slop-core",
        ] {
            self.inputs.absent(root)?;
        }
        Ok(true)
    }
    /// Fingerprint the complete native inventory from the held candidate tree.
    pub fn native_source_fingerprint(&mut self) -> Result<Option<String>> {
        if let Some(value) = &self.native_fingerprint {
            return Ok(value.clone());
        }
        use conductor_native::source_fingerprint::{self, Fingerprint, EXTRA_FILES, SOURCE_TREES};
        let mut paths: Vec<String> = self
            .inputs
            .entries
            .keys()
            .filter(|path| source_fingerprint::includes(path))
            .cloned()
            .collect();
        // The build and live inventories sort Path components, not UTF-8 strings.
        paths.sort_by(|left, right| Path::new(left).cmp(Path::new(right)));
        if EXTRA_FILES
            .iter()
            .any(|path| !self.inputs.entries.contains_key(*path))
            || SOURCE_TREES.iter().any(|name| {
                let base = format!("native/{name}/");
                ["Cargo.toml", "Cargo.lock"]
                    .iter()
                    .any(|file| !self.inputs.entries.contains_key(&format!("{base}{file}")))
                    || !paths
                        .iter()
                        .any(|path| path.starts_with(&format!("{base}src/")))
            })
        {
            self.native_fingerprint = Some(None);
            return Ok(None);
        }
        ensure!(
            paths.len() <= 4096,
            "native source inventory exceeds 4096 files"
        );
        let expected: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        ensure!(
            source_fingerprint::files(&self.snapshot.path())? == expected,
            "materialized native source inventory differs from candidate tree"
        );
        let mut fingerprint = Fingerprint::new();
        for path in paths {
            let entry = &self.inputs.entries[&path];
            ensure!(
                matches!(entry.mode.as_str(), "100644" | "100755") && entry.object_type == "blob",
                "native source is not a regular file: {path}"
            );
            let bytes = self.read(&path, 2 * 1024 * 1024)?;
            fingerprint.add(Path::new(&path), &bytes)?;
        }
        let value = Some(fingerprint.finish());
        ensure!(
            source_fingerprint::files(&self.snapshot.path())? == expected,
            "materialized native source inventory changed during capture"
        );
        self.native_inventory = Some(expected);
        self.native_fingerprint = Some(value.clone());
        Ok(value)
    }
    pub fn new(
        source: &Source,
        snapshot: &'a Snapshot,
        candidate: &Candidate,
        policy: &'a Policy,
        today: &'a str,
    ) -> Result<Self> {
        Ok(Self {
            inputs: Inputs::new(source, snapshot, candidate, policy, today)?,
            ledger: BTreeMap::new(),
            copied_bytes: 0,
            native_fingerprint: None,
            native_inventory: None,
            cargo_inventories: Vec::new(),
            supporting: None,
            workspace: None,
            source_root: source.root().to_owned(),
            snapshot,
            failed: false,
        })
    }
    pub fn read(&mut self, path: &str, limit: usize) -> Result<Vec<u8>> {
        let captured = match self.inputs.read_bytes_with_limit(path, limit) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        self.copied_bytes = self
            .copied_bytes
            .checked_add(captured.len())
            .context("mutation capture work overflow")?;
        if self.copied_bytes > 32 * 1024 * 1024 {
            self.failed = true;
            anyhow::bail!("mutation capture copy work exceeds32MiB");
        }
        let bytes = captured.to_vec();
        let entry = self
            .inputs
            .entries
            .get(path)
            .context("mutation input entry unavailable")?;
        self.ledger.entry(path.into()).or_insert_with(|| {
            json!({"path":path,"git_blob_oid":entry.oid,
            "sha256":hex(&bytes),"bytes":bytes.len()})
        });
        Ok(bytes)
    }
    pub fn text(&mut self, path: &str, limit: usize) -> Result<String> {
        String::from_utf8(self.read(path, limit)?).context("mutation evidence input must be UTF-8")
    }
    pub fn json(&mut self, path: &str) -> Result<Value> {
        crate::tool_schema::parse_document_with_limit(
            &self.text(path, 2 * 1024 * 1024)?,
            2 * 1024 * 1024,
        )
    }
    pub fn optional_hash(&mut self, path: &str) -> Result<Option<String>> {
        crate::candidate_source::safe_path(path.as_bytes())?;
        if !self.inputs.entries.contains_key(path) {
            self.inputs.absent(path)?;
            return Ok(None);
        }
        Ok(Some(hex(&self.read(path, 1024 * 1024)?)))
    }
    pub fn config(&mut self) -> Result<Option<conductor_native::project_config::ProjectConfig>> {
        if self.inputs.entries.contains_key("forge.toml") {
            return conductor_native::project_config::parse_native(
                Path::new("forge.toml"),
                &self.text("forge.toml", 65536)?,
            )
            .map(Some)
            .map_err(anyhow::Error::msg);
        }
        self.inputs.absent("forge.toml")?;
        if self.inputs.entries.contains_key("pyproject.toml") {
            return conductor_native::project_config::parse_legacy(
                Path::new("pyproject.toml"),
                &self.text("pyproject.toml", 65536)?,
            )
            .map(Some)
            .map_err(anyhow::Error::msg);
        }
        self.inputs.absent("pyproject.toml")?;
        Ok(None)
    }
    /// Build a Git-derived inventory, compare it with the held materialization,
    /// and return the held scanner for the caller to retain after validation.
    pub fn cargo_inventory(
        &mut self,
        declaration: &str,
        selected: &[String],
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<crate::mutation_cargo_capture::Held> {
        use conductor_native::mutation_cargo_inventory::{self as cargo, Entry};
        let budget = cargo_budget(&self.cargo_inventories)?;
        let mut relevant_count = 0usize;
        let mut map = BTreeMap::new();
        let entries: Vec<_> = self.inputs.entries.values().cloned().collect();
        ensure!(
            entries.len() <= 16384,
            "Cargo candidate inventory exceeds 16384 entries"
        );
        for entry in entries {
            let path = &entry.path;
            let mut parent = Path::new(path).parent();
            while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
                let dir = dir.to_str().context("Cargo directory must be UTF-8")?;
                map.entry(dir.to_owned()).or_insert(Entry::Directory);
                parent = Path::new(dir).parent();
            }
            let regular =
                entry.object_type == "blob" && matches!(entry.mode.as_str(), "100644" | "100755");
            let value = if !regular {
                Entry::SymlinkOrSpecial
            } else if !cargo::excluded(path) && (cargo::relevant(path) || path == declaration) {
                relevant_count += 1;
                ensure!(
                    relevant_count <= budget,
                    "Cargo held file budget exceeds 256"
                );
                if !files.contains_key(path) {
                    files.insert(path.clone(), self.read(path, 2 * 1024 * 1024)?);
                }
                Entry::Regular(Some(files[path].clone()))
            } else {
                Entry::Regular(None)
            };
            ensure!(
                map.insert(path.clone(), value).is_none(),
                "Cargo Git inventory path collision"
            );
            ensure!(
                map.len() <= 16384,
                "Cargo candidate inventory exceeds 16384 entries"
            );
        }
        let declared = files
            .get(declaration)
            .context("Cargo declaration missing from candidate")?;
        let git_inventory = cargo::build(declared, selected, &map).map_err(anyhow::Error::msg)?;
        let held = crate::mutation_cargo_capture::Held::capture_with_limit(
            &self.snapshot.path(),
            declaration,
            selected,
            budget,
        )?;
        ensure!(
            held.file_count() <= budget,
            "Cargo held file budget exceeded"
        );
        ensure!(
            &git_inventory == held.inventory() && declared.as_slice() == held.declaration_bytes(),
            "Cargo materialization differs from captured Git inventory"
        );
        Ok(held)
    }
    pub fn retain_cargo(&mut self, held: crate::mutation_cargo_capture::Held) {
        self.cargo_inventories.push(held);
    }
    pub fn supporting_inventory(
        &mut self,
        required: &BTreeMap<String, String>,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        use conductor_native::mutation_cargo_inventory::{self as cargo, Entry};
        let budget = cargo_budget(&self.cargo_inventories)?;
        let mut map = BTreeMap::new();
        let entries: Vec<_> = self.inputs.entries.values().cloned().collect();
        ensure!(
            entries.len() <= 16384,
            "supporting Git inventory exceeds 16384 entries"
        );
        let mut relevant = 0usize;
        for entry in entries {
            let path = &entry.path;
            let mut parent = Path::new(path).parent();
            while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
                map.entry(
                    dir.to_str()
                        .context("supporting path must be UTF-8")?
                        .to_owned(),
                )
                .or_insert(Entry::Directory);
                parent = dir.parent();
            }
            let regular =
                entry.object_type == "blob" && matches!(entry.mode.as_str(), "100644" | "100755");
            let value = if !regular {
                Entry::SymlinkOrSpecial
            } else if !cargo::excluded(path) && cargo::relevant(path) {
                relevant += 1;
                ensure!(relevant <= budget, "supporting file budget exceeded");
                Entry::Regular(Some(self.read(path, 2 * 1024 * 1024)?))
            } else {
                Entry::Regular(None)
            };
            ensure!(
                map.insert(path.clone(), value).is_none(),
                "supporting path collision"
            );
            ensure!(map.len() <= 16384, "supporting path budget exceeded");
        }
        let expected = conductor_native::mutation_cargo_supporting::build(&map, required)
            .map_err(anyhow::Error::msg)?;
        let held = crate::mutation_cargo_supporting::Held::capture(
            &self.snapshot.path(),
            budget,
            required,
        )?;
        ensure!(
            held.inventory() == &expected,
            "materialized supporting inventory differs from Git tree"
        );
        self.supporting = Some(held);
        Ok(expected)
    }
    pub fn workspace_inventory(
        &mut self,
        required: &crate::mutation_cargo_supporting::WorkspaceRequired,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        // Shared full-snapshot domain for Cargo workspace and confined Fest profiles.
        // The verified contract selects this path; a receipt cannot select its inputs.
        let config = self.config()?;
        crate::mutation_workspace_policy::manifest_in_namespace(
            config.as_ref(),
            &required.manifest,
        )?;
        let protected = required.pins.keys().cloned().collect::<Vec<_>>();
        let selection = crate::mutation_workspace_policy::selection(config.as_ref(), &protected)?;
        if let Some((existing, identity)) = &self.workspace {
            ensure!(*existing == selection, "workspace output selection changed");
            return Ok(identity.clone());
        }
        let source = Source::open(&self.source_root)?;
        let git = crate::mutation_snapshot::git_input_identity(
            &source,
            &self.snapshot.report().tree_oid,
            &selection,
        )
        .map_err(anyhow::Error::msg)?;
        let materialized = crate::mutation_snapshot::materialized_input_identity(
            &self.snapshot.path(),
            &selection,
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            git == materialized,
            "candidate workspace materialization differs from Git inputs"
        );
        self.workspace = Some((selection, git.clone()));
        Ok(git)
    }

    pub fn verify(&self) -> Result<()> {
        ensure!(
            !self.failed,
            "mutation capture admission failed; no complete observation available"
        );
        if let Some(expected) = &self.native_inventory {
            ensure!(
                &conductor_native::source_fingerprint::files(&self.snapshot.path())? == expected,
                "materialized native source inventory changed"
            );
        }
        if let Some((selection, identity)) = &self.workspace {
            let current = crate::mutation_snapshot::materialized_input_identity(
                &self.snapshot.path(),
                selection,
            )
            .map_err(anyhow::Error::msg)?;
            ensure!(
                &current == identity,
                "candidate workspace materialization changed"
            );
        }
        for held in &self.cargo_inventories {
            held.verify()?;
        }
        if let Some(held) = &self.supporting {
            held.verify()?;
        }
        self.inputs.verify()
    }
}
