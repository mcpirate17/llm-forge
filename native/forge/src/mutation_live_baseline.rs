//! Explicit external baseline input with named-parent and byte revalidation.
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub(crate) struct External {
    parent: DirectoryBinding,
    root: PathBuf,
    name: String,
    file: Option<BoundFile>,
    bytes: Option<Vec<u8>>,
}
impl External {
    pub(crate) fn open(path: &Path, source: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute() && !path.starts_with(source),
            "external baseline must be absolute outside source"
        );
        let root = path
            .parent()
            .context("baseline parent missing")?
            .canonicalize()?;
        ensure!(
            root.join(path.file_name().context("baseline filename missing")?) == path,
            "baseline path contains a symlink or is not normalized"
        );
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("baseline name must be UTF-8")?
            .to_owned();
        crate::candidate_source::safe_path(name.as_bytes())?;
        let parent = DirectoryBinding::open(&root, Some(source))?;
        let (file, bytes) = match fs::symlink_metadata(path) {
            Ok(_) => {
                let (held, bytes) = candidate_inputs::observed(path, 2 * 1024 * 1024, source)?;
                (Some(held), Some(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(error) => return Err(error.into()),
        };
        let this = Self {
            parent,
            root,
            name,
            file,
            bytes,
        };
        this.verify()?;
        Ok(this)
    }
    pub(crate) fn value(&self) -> Result<Option<serde_json::Value>> {
        self.bytes
            .as_deref()
            .map(|bytes| {
                crate::tool_schema::parse_document_with_limit(
                    std::str::from_utf8(bytes)?,
                    2 * 1024 * 1024,
                )
            })
            .transpose()
    }
    pub(crate) fn hash(&self) -> Option<String> {
        self.bytes
            .as_ref()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.parent.verify()?;
        if let Some(file) = &self.file {
            file.verify()?
        } else {
            match fs::symlink_metadata(self.root.join(&self.name)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!("previously absent external baseline appeared"),
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn missing_and_present_external_baselines_are_revalidated() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let outside = tmp.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
        let path = outside.join("baseline.json");
        let missing = External::open(&path, &source).unwrap();
        assert!(missing.value().unwrap().is_none());
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(missing.verify().is_err());
        let present = External::open(&path, &source).unwrap();
        assert!(present.value().unwrap().unwrap().is_object());
        fs::write(&path, b"{\"changed\":true}").unwrap();
        assert!(present.verify().is_err());
    }
}
