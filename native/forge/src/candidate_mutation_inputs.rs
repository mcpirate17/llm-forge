//! Reuse held candidate inputs for every registry/config/source/test/receipt byte.
use super::*;
pub(crate) struct Capture<'a> {
    pub inputs: Inputs<'a>,
    pub ledger: BTreeMap<String, Value>,
    copied_bytes: usize,
    pub failed: bool,
}
impl<'a> Capture<'a> {
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
    pub fn verify(&self) -> Result<()> {
        ensure!(
            !self.failed,
            "mutation capture admission failed; no complete observation available"
        );
        self.inputs.verify()
    }
}
