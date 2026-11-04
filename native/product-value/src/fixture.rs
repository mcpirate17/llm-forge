//! Synthetic existing bin directories only: no actual virtualenv is touched.
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub struct Fixture {
    pub source: PathBuf,
    pub destination: PathBuf,
    scripts: usize,
    body: Vec<u8>,
    source_image: BTreeMap<String, (u64, u64, u32, String)>,
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub const SKIPPED: [(&str, &[u8]); 4] = [
    (
        "python3.12",
        b"\x7fELF\0metadata-only fixture; never executed",
    ),
    ("foreign", b"#!/usr/bin/python3\nunchanged\n"),
    ("shell", b"#!/bin/sh\nunchanged\n"),
    ("binary", b"\x7fELF\0not a script"),
];
pub fn body_bytes(size: usize) -> Vec<u8> {
    let mut body = b"print('fixture only; never executed')\n".to_vec();
    while body.len() < size {
        body.extend_from_slice(b"# bounded fixture padding\n");
    }
    body.truncate(size);
    body
}
pub fn script_bytes(source: &Path, index: usize, body: &[u8]) -> Vec<u8> {
    let ending = if index.is_multiple_of(2) {
        "\n"
    } else {
        "\r\n"
    };
    let mut data = format!("#!{}{ending}", source.join("bin/python3.12").display()).into_bytes();
    data.extend_from_slice(body);
    data
}
fn private(path: &Path) -> Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
impl Fixture {
    pub fn provenance(&self) -> serde_json::Value {
        serde_json::json!({"source_path":self.source,"destination_path":self.destination,
            "source_image_dev_inode_mode_sha256":self.source_image,"body_sha256":hash(&self.body),
            "hardlinks_reset_before_each_candidate":true,"script_modes":"0751","shebangs":"alternating LF/CRLF; exact source Python3.12 alias", "symlink":{"alias":"script-0000"}})
    }
    pub fn new(root: &Path, scripts: usize, body_size: usize) -> Result<Self> {
        let source = root.join("source-env");
        let destination = root.join("guest-env");
        private(&source)?;
        private(&destination)?;
        private(&source.join("bin"))?;
        private(&destination.join("bin"))?;
        let body = body_bytes(body_size);
        fs::write(source.join("bin").join(SKIPPED[0].0), SKIPPED[0].1)?;
        for index in 0..scripts {
            let data = script_bytes(&source, index, &body);
            let path = source.join(format!("bin/script-{index:04}"));
            fs::write(&path, data)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o751))?;
        }
        for (name, bytes) in &SKIPPED[1..] {
            fs::write(source.join("bin").join(name), bytes)?;
        }
        symlink("script-0000", source.join("bin/alias"))?;
        let mut value = Self {
            source,
            destination,
            scripts,
            body,
            source_image: BTreeMap::new(),
        };
        value.source_image = value.image()?;
        value.reset()?;
        Ok(value)
    }
    fn image(&self) -> Result<BTreeMap<String, (u64, u64, u32, String)>> {
        let mut image = BTreeMap::new();
        for entry in fs::read_dir(self.source.join("bin"))? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_file() {
                image.insert(
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("fixture UTF8"))?,
                    (meta.dev(), meta.ino(), meta.mode(), hash(&fs::read(path)?)),
                );
            }
        }
        Ok(image)
    }
    pub fn reset(&self) -> Result<()> {
        ensure!(self.image()? == self.source_image, "source fixture changed");
        for entry in fs::read_dir(self.destination.join("bin"))? {
            fs::remove_file(entry?.path())?;
        }
        for entry in fs::read_dir(self.source.join("bin"))? {
            let entry = entry?;
            let name = entry.file_name();
            let destination = self.destination.join("bin").join(&name);
            if entry.file_type()?.is_symlink() {
                symlink(fs::read_link(entry.path())?, destination)?;
            } else {
                fs::hard_link(entry.path(), destination)?;
            }
        }
        Ok(())
    }
    pub fn verify(&self, stdout: &[u8], changed: usize) -> Result<()> {
        ensure!(
            stdout == format!("relocated {changed} Python console script(s)\n").as_bytes(),
            "unexpected relocated count/output: {}",
            String::from_utf8_lossy(stdout)
        );
        ensure!(
            self.image()? == self.source_image,
            "source inode/mode/bytes changed"
        );
        for index in 0..self.scripts {
            let name = format!("script-{index:04}");
            let path = self.destination.join("bin").join(&name);
            let mut expected =
                format!("#!{}\n", self.destination.join("bin/python3.12").display()).into_bytes();
            expected.extend_from_slice(&self.body);
            ensure!(
                fs::read(&path)? == expected,
                "destination bytes mismatch: {name}"
            );
            let meta = fs::metadata(&path)?;
            let source = fs::metadata(self.source.join("bin").join(name))?;
            ensure!(
                meta.mode() & 0o777 == 0o751
                    && (meta.dev(), meta.ino()) != (source.dev(), source.ino()),
                "mode/inode copy-on-write mismatch"
            );
        }
        for name in ["python3.12", "foreign", "shell", "binary"] {
            ensure!(
                fs::read(self.source.join("bin").join(name))?
                    == fs::read(self.destination.join("bin").join(name))?,
                "skipped entry changed: {name}"
            );
        }
        ensure!(
            fs::read_link(self.destination.join("bin/alias"))? == Path::new("script-0000"),
            "symlink changed"
        );
        Ok(())
    }
    pub fn destination_inodes(&self) -> Result<BTreeMap<String, (u64, u64)>> {
        fs::read_dir(self.destination.join("bin"))?
            .map(|entry| {
                let entry = entry?;
                let meta = fs::symlink_metadata(entry.path())?;
                Ok((
                    entry.file_name().to_string_lossy().into_owned(),
                    (meta.dev(), meta.ino()),
                ))
            })
            .collect()
    }
}
