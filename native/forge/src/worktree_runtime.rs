//! Repair console-script shebangs in two existing virtualenvs without an interpreter.
//!
//! Both bin directories are pinned and cooperatively flocked. Script symlinks
//! are ignored; only interpreter-name discovery may follow metadata, never bytes.
//! Same-UID writers ignoring directory flocks can race checks and rename; failed
//! post-exchange checks retain displaced bytes rather than overwriting them.
#[path = "worktree_runtime_paths.rs"]
mod paths;
#[path = "worktree_runtime_publish.rs"]
mod publication;
use anyhow::{ensure, Context, Result};
use paths::{Bin, Interpreters, Stamp, MAX_TOTAL};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

/// Match the entire first shebang line, including the source interpreter name.
/// Preserve legacy CRLF behavior: the replacement line is LF, rest is unchanged.
fn rewrite(
    payload: &[u8],
    replacements: &BTreeMap<Vec<u8>, OsString>,
    destination: &Path,
) -> Option<Vec<u8>> {
    let newline = payload.iter().position(|byte| *byte == b'\n')?;
    let first = &payload[..newline];
    if first.contains(&0) {
        return None;
    }
    let end = first
        .iter()
        .rposition(|byte| *byte != b'\r')
        .map_or(0, |at| at + 1);
    let name = replacements.get(&first[..end])?;
    let mut out = b"#!".to_vec();
    out.extend_from_slice(destination.join(name).as_os_str().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&payload[newline + 1..]);
    Some(out)
}
fn regular(bin: &Bin, name: &OsStr) -> Result<Option<Stamp>> {
    let metadata = match bin.dir.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect launcher entry"),
    };
    Ok(metadata.is_file().then(|| Stamp::capture(&metadata)))
}

/// Copy-on-write exact source-Python launcher shebangs into an existing guest.
/// No venv/worktree is created and no interpreter is executed. Discovery reads
/// metadata only; regular candidates (even named python3) receive bounded probes.
/// Returns normalized absolute destination paths; a second invocation changes none.
/// Errors can follow earlier successful paths and include that completed list.
pub fn relocate_console_scripts(
    source_venv: &Path,
    destination_venv: &Path,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<Vec<PathBuf>> {
    cancel()?;
    let source = Bin::open(source_venv)?;
    let destination = Bin::open(destination_venv)?;
    source.distinct(&destination)?;
    // Nonblocking leases avoid deadlock if another invocation reverses operands.
    let _source_lease = source.lease()?;
    let _destination_lease = destination.lease()?;
    source.verify()?;
    destination.verify()?;
    let (names, source_directory) = paths::names(&source)?;
    let interpreters = Interpreters::discover(&source, &names)?;
    let mut relocated = Vec::new();
    let mut total = 0usize;
    let result = (|| -> Result<()> {
        for name in names {
            cancel()?;
            source.verify()?;
            destination.verify()?;
            let Some(original_source) = regular(&source, &name)? else {
                continue;
            };
            let Some(original_destination) = regular(&destination, &name)? else {
                continue;
            };
            let mut script = publication::Script::open(&destination, &name, &original_destination)?;
            let prefix = script.prefix()?;
            script.verify(&destination, &name)?;
            if prefix.contains(&0)
                || rewrite(&prefix, &interpreters.replacements, &destination.path).is_none()
            {
                continue;
            }
            let bytes = script.bytes(&destination, &name)?;
            let rewritten = rewrite(&bytes, &interpreters.replacements, &destination.path)
                .context("launcher shebang changed after prefix capture")?;
            total = total
                .checked_add(bytes.len())
                .and_then(|value| value.checked_add(rewritten.len()))
                .context("launcher byte budget overflow")?;
            ensure!(
                total <= MAX_TOTAL,
                "launcher input/output byte budget exceeds 2 GiB"
            );
            let mut before_publish = || -> Result<()> {
                cancel()?;
                source.verify()?;
                destination.verify()?;
                interpreters.verify(&source)?;
                ensure!(
                    Stamp::capture(&source.dir.dir_metadata()?) == source_directory,
                    "source bin inventory changed"
                );
                ensure!(
                    regular(&source, &name)?.as_ref() == Some(&original_source),
                    "source launcher entry changed"
                );
                Ok(())
            };
            publication::replace(
                &destination,
                &name,
                &script,
                &bytes,
                &rewritten,
                &mut before_publish,
            )
            .with_context(|| format!("relocate {}", destination.path.join(&name).display()))?;
            relocated.push(destination.path.join(name));
        }
        Ok(())
    })();
    result.with_context(|| {
        format!(
            "relocation stopped; completed {} earlier script(s): {relocated:?}",
            relocated.len()
        )
    })?;
    Ok(relocated)
}

#[cfg(test)]
#[path = "../tests/support/runtime_relocation.rs"]
mod fixture;
#[cfg(test)]
#[path = "worktree_runtime_tests.rs"]
mod tests;
