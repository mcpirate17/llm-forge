//! Bind granted probe executables through their observed bytes and named paths.
use crate::{
    candidate_engine_identity::{self, BinaryIdentity},
    candidate_execution_plan::Selection,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
struct Program {
    named: PathBuf,
    target: PathBuf,
    identity: BinaryIdentity,
}
impl Program {
    fn capture(path: &Path) -> Result<Self> {
        let target = path.canonicalize()?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&target)?;
        ensure!(
            file.metadata()?.permissions().mode() & 0o111 != 0,
            "granted probe program is not executable"
        );
        let identity = candidate_engine_identity::executable_file(file, || Ok(()))?;
        let held = Self {
            named: path.into(),
            target,
            identity,
        };
        held.verify()?;
        Ok(held)
    }
    fn interpreter(&self) -> Result<Option<PathBuf>> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.target)?;
        let mut prefix = [0u8; 256];
        let read = file.read(&mut prefix)?;
        self.verify()?;
        if !prefix[..read].starts_with(b"#!") {
            return Ok(None);
        }
        let line = &prefix[2..read];
        let end = match line.iter().position(|byte| *byte == b'\n') {
            Some(end) => end,
            None if read < prefix.len() => line.len(),
            None => anyhow::bail!("probe program shebang exceeds 256 bytes"),
        };
        let text = std::str::from_utf8(&line[..end]).context("probe shebang must be UTF-8")?;
        let program = text
            .split_ascii_whitespace()
            .next()
            .context("probe shebang interpreter missing")?;
        let path = PathBuf::from(program);
        ensure!(
            path.is_absolute(),
            "probe shebang interpreter must be absolute"
        );
        ensure!(
            path.file_name().is_none_or(|name| name != "env"),
            "env-based probe shebang is not an explicit interpreter grant"
        );
        Ok(Some(path))
    }
    fn verify(&self) -> Result<()> {
        ensure!(
            self.named.canonicalize()? == self.target,
            "granted program path changed"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.target)?;
        ensure!(
            candidate_engine_identity::executable_file(file, || Ok(()))? == self.identity,
            "granted program bytes or identity changed"
        );
        Ok(())
    }
}
fn capture_chain(
    path: &Path,
    targets: &mut BTreeSet<PathBuf>,
    items: &mut Vec<Program>,
    bytes: &mut u64,
) -> Result<()> {
    let mut path = path.to_owned();
    for _ in 0..5 {
        let item = Program::capture(&path)?;
        if !targets.insert(item.target.clone()) {
            return Ok(());
        }
        *bytes = bytes
            .checked_add(item.identity.bytes)
            .context("probe executable byte count overflow")?;
        ensure!(
            *bytes <= 2 * 1024 * 1024 * 1024,
            "probe executable aggregate exceeds 2 GiB"
        );
        let interpreter = item.interpreter()?;
        items.push(item);
        match interpreter {
            Some(next) => path = next,
            None => return Ok(()),
        }
    }
    anyhow::bail!("probe interpreter chain exceeds four levels")
}
pub(super) struct Programs {
    items: Vec<Program>,
}
impl Programs {
    pub fn capture(selection: &Selection, skip_pytest: bool) -> Result<Self> {
        let mut names = BTreeSet::new();
        for row in selection.policy.document["tools"]
            .as_array()
            .context("policy tools missing")?
        {
            if row["required_profiles"]
                .as_array()
                .context("tool profiles missing")?
                .iter()
                .any(|v| v.as_str() == Some(selection.profile()))
            {
                let name = row["executable"]
                    .as_str()
                    .context("tool executable missing")?;
                if selection.grants.programs.contains_key(name) {
                    names.insert(name.to_owned());
                }
            }
        }
        if let Some(name) = selection.grants.python.as_ref().filter(|_| !skip_pytest) {
            names.insert(name.clone());
        }
        ensure!(names.len() <= 128, "probe executable count exceeds 128");
        let mut bytes = 0u64;
        let mut items = Vec::new();
        let mut targets = BTreeSet::new();
        for name in names {
            let path = selection
                .grants
                .programs
                .get(&name)
                .context("probe executable grant missing")?;
            capture_chain(path, &mut targets, &mut items, &mut bytes)?;
        }
        Ok(Self { items })
    }
    pub fn verify(&self) -> Result<()> {
        for item in &self.items {
            item.verify()?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };
    #[test]
    fn executable_bytes_and_symlink_target_are_revalidated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let first = root.join("first");
        let second = root.join("second");
        let named = root.join("tool");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::set_permissions(&first, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&second, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&first, &named).unwrap();
        let pin = Program::capture(&named).unwrap();
        pin.verify().unwrap();
        fs::write(&first, b"changed").unwrap();
        assert!(pin.verify().is_err());
        fs::remove_file(&named).unwrap();
        symlink(&second, &named).unwrap();
        assert!(pin.verify().is_err());
    }
    #[test]
    fn shebang_interpreter_is_bound_and_env_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("script");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = BTreeSet::new();
        let mut items = Vec::new();
        let mut bytes = 0;
        capture_chain(&script, &mut paths, &mut items, &mut bytes).unwrap();
        assert_eq!(items.len(), 2);
        for item in &items {
            item.verify().unwrap();
        }
        fs::write(&script, b"#!/usr/bin/env python\n").unwrap();
        assert!(items[0].verify().is_err());
        assert!(Program::capture(&script).unwrap().interpreter().is_err());
    }
}
