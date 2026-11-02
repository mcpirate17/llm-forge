//! One manifest algorithm with bounded ambient/captured source readers.
use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;
pub(super) trait CampaignReader {
    fn resolve(&self, path: &Path) -> Result<PathBuf, String>;
    fn read(&self, path: &Path) -> Result<Vec<u8>, String>;
    fn hash(&self, path: &Path) -> Option<String> {
        self.read(path)
            .ok()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    }
    fn patch_paths(&self, path: &Path) -> Result<Vec<String>, String> {
        let bytes = self.read(path)?;
        let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        parse_patch_paths(text)
    }
    fn inventory(&self, root: &Path, path: &str, kind: &str) -> Result<Vec<String>, String> {
        let bytes = self.read(&root.join(path))?;
        let source = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        match kind {
            "python_ast" => inventory_python_source(source, path),
            "cargo_test" => inventory_rust_source(source, path),
            "c_test" => inventory_c_source(source, path),
            _ => Err(format!("unsupported complete test inventory: {kind}")),
        }
    }
}
pub(super) struct FsCampaignReader {
    bytes: Cell<usize>,
}
impl FsCampaignReader {
    pub fn new() -> Self {
        Self {
            bytes: Cell::new(0),
        }
    }
}
impl CampaignReader for FsCampaignReader {
    fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        Ok(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        let bytes = crate::mutation_evidence::inputs::read_bounded(path, 32 * 1024 * 1024)?
            .ok_or_else(|| format!("captured campaign input missing: {}", path.display()))?;
        let total = self
            .bytes
            .get()
            .checked_add(bytes.len())
            .ok_or("campaign read overflow")?;
        if total > 64 * 1024 * 1024 {
            return Err("campaign ambient reads exceed64MiB".into());
        }
        self.bytes.set(total);
        Ok(bytes)
    }
}
struct Captured<'a> {
    files: &'a BTreeMap<String, Vec<u8>>,
}
impl CampaignReader for Captured<'_> {
    fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        lexical_absolute(path)
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        let relative = path
            .strip_prefix("/")
            .map_err(|_| "captured campaign path is not absolute")?
            .to_str()
            .ok_or("captured campaign path must be UTF-8")?;
        self.files
            .get(relative)
            .cloned()
            .ok_or_else(|| format!("captured campaign input missing: {relative}"))
    }
}
pub(crate) fn load_captured(
    path: &str,
    files: &BTreeMap<String, Vec<u8>>,
    selected: &[String],
) -> Result<Value, String> {
    if files.len() > 4096 || selected.len() > 4096 {
        return Err("captured campaign inventory exceeds4096".into());
    }
    let mut bytes = 0usize;
    for (path, value) in files {
        safe_relative(path, "captured campaign input")?;
        bytes = bytes
            .checked_add(value.len())
            .ok_or("campaign capture byte overflow")?;
        if path.len() > 4096 || value.len() > 2 * 1024 * 1024 || bytes > 32 * 1024 * 1024 {
            return Err("captured campaign byte budget exceeded".into());
        }
    }
    let path = safe_relative(path, "captured campaign manifest")?;
    let manifest = files
        .get(&path)
        .ok_or("captured campaign manifest missing")?;
    let parsed = crate::mutation_evidence::parse_request(
        std::str::from_utf8(manifest).map_err(|error| error.to_string())?,
    )?;
    let mut symbols = HashMap::new();
    if let Some(sources) = parsed.get("source_symbols").and_then(Value::as_object) {
        for raw_source in sources.keys() {
            let source = safe_relative(raw_source, "symbol source")?;
            let bytes = files
                .get(&source)
                .ok_or_else(|| format!("symbol source unavailable: {source}"))?;
            let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            let actual = crate::candidate_python_symbol_hash::hashes(text)?;
            symbols.insert(source.clone(), actual.into_iter().collect());
        }
    }
    let selected = selected.iter().cloned().collect();
    let reader = Captured { files };
    let contract = super::legacy::load(
        Path::new("/"),
        &path,
        &HashMap::new(),
        &symbols,
        &selected,
        true,
        &reader,
    )?;
    serde_json::to_value(contract).map_err(|error| error.to_string())
}
