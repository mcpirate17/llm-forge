//! Hold the exact seven-day native cost-ledger inputs and their absences.
use crate::mutation_live_inputs::LiveInputs;
use anyhow::{ensure, Context, Result};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub(super) struct LedgerInputs {
    inputs: LiveInputs,
    root: PathBuf,
    day: i64,
    environment: [Option<OsString>; 3],
}
fn day() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() / 86_400,
    )?)
}
fn environment() -> [Option<OsString>; 3] {
    [
        std::env::var_os("LEDGER_ROOT"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    ]
}
fn root(cwd: &Path) -> Result<PathBuf> {
    let path = conductor_native::state_paths::ledger_root(None)?;
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "ledger path must be normalized"
    );
    Ok(path)
}
fn anchor(path: &Path) -> Result<PathBuf> {
    let mut candidate = path;
    loop {
        match fs::symlink_metadata(candidate) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir()
                        && !meta.file_type().is_symlink()
                        && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() })
                        && meta.mode() & 0o002 == 0,
                    "ledger ancestor is not a safe directory"
                );
                ensure!(
                    candidate.canonicalize()? == candidate,
                    "ledger ancestor contains a symlink"
                );
                return Ok(candidate.into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().context("ledger ancestor missing")?
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn capture_window(inputs: &mut LiveInputs, prefix: &str, day: i64) -> Result<()> {
    for offset in 0..7 {
        let (year, month, date) = crate::civil::civil_from_days(day - offset);
        for table in [
            "hook_rollup",
            "session_rollup",
            "agent_rollup",
            "task_dispatch",
        ] {
            let name = format!("{table}/{year:04}-{month:02}-{date:02}.jsonl");
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let _ = inputs.read(&path)?;
        }
    }
    Ok(())
}
impl LedgerInputs {
    pub fn capture(cwd: &Path) -> Result<Self> {
        let day = day()?;
        let environment = environment();
        let root = root(cwd)?;
        let anchor = anchor(&root)?;
        let prefix = root
            .strip_prefix(&anchor)?
            .to_str()
            .context("ledger path must be UTF-8")?;
        let mut inputs = LiveInputs::open(&anchor)?;
        capture_window(&mut inputs, prefix, day)?;
        let held = Self {
            inputs,
            root,
            day,
            environment,
        };
        held.verify(cwd)?;
        Ok(held)
    }
    pub fn verify(&self, cwd: &Path) -> Result<()> {
        ensure!(
            day()? == self.day && environment() == self.environment && root(cwd)? == self.root,
            "ledger date, environment or root changed during gate"
        );
        self.inputs.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seven_day_window_rechecks_observed_rows_and_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("hook_rollup")).unwrap();
        let now = day().unwrap();
        let (year, month, date) = crate::civil::civil_from_days(now);
        let path = root.join(format!("hook_rollup/{year:04}-{month:02}-{date:02}.jsonl"));
        fs::write(&path, b"{}\n").unwrap();
        let mut inputs = LiveInputs::open(&root).unwrap();
        capture_window(&mut inputs, "", now).unwrap();
        inputs.verify().unwrap();
        fs::write(&path, b"{\"changed\":true}\n").unwrap();
        assert!(inputs.verify().is_err());
    }
}
