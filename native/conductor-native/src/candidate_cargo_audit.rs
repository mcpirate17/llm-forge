//! Bounded Cargo.lock ownership in a caller-pinned, immutable candidate snapshot.
//! No environment, process, manifest/workspace inference or ambient path reopening.
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use serde::Serialize;
use std::{collections::BTreeSet, io};

const MAX_INPUTS: usize = 100_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_PROBES: usize = 1_000_000;
const MAX_LOCKS: usize = 256;
const MAX_PLAN_BYTES: usize = 24 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Ownership {
    pub selected: String,
    pub lockfile: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuditPlan {
    pub lockfiles: Vec<String>,
    pub ownership: Vec<Ownership>,
}
struct Budget {
    probes: usize,
}
impl Budget {
    fn probe(&mut self) -> Result<(), String> {
        if self.probes >= MAX_PROBES {
            return Err("cargo-audit metadata/work probe limit exceeded".into());
        }
        self.probes += 1;
        Ok(())
    }
}

/// Every selected input must be a readable regular file and own a readable lock.
/// The caller must keep the pinned root and descendants immutable throughout use.
/// This is confinement during preparation, not a custom VFS or post-return lease.
pub fn plan(root: &Dir, selected: &[&str]) -> Result<AuditPlan, String> {
    admit(selected)?;
    let mut inputs = selected.to_vec();
    inputs.sort_unstable();
    inputs.dedup();
    let mut locks = BTreeSet::new();
    let mut ownership = Vec::with_capacity(inputs.len());
    let mut budget = Budget { probes: 0 };
    let mut bytes = 0usize;
    for selected in inputs {
        let lockfile = owner(root, selected, &mut budget)?;
        let new = !locks.contains(&lockfile);
        if new && locks.len() >= MAX_LOCKS {
            return Err("cargo-audit distinct lockfile limit exceeded".into());
        }
        bytes = bytes
            .checked_add(selected.len())
            .and_then(|n| n.checked_add(lockfile.len()))
            .and_then(|n| n.checked_add(if new { lockfile.len() } else { 0 }))
            .ok_or("cargo-audit plan size overflow")?;
        if bytes > MAX_PLAN_BYTES {
            return Err("cargo-audit plan byte limit exceeded".into());
        }
        if new {
            locks.insert(lockfile.clone());
        }
        ownership.push(Ownership {
            selected: selected.to_owned(),
            lockfile,
        });
    }
    let plan = AuditPlan {
        lockfiles: locks.into_iter().collect(),
        ownership,
    };
    serialized_size(&plan, MAX_JSON_BYTES)?;
    Ok(plan)
}

/// Measure actual compact JSON encoding, including escapes and punctuation,
/// without allocating an encoded buffer. Raw cloned paths have a separate cap.
pub(crate) fn serialized_size(value: &impl Serialize, maximum: usize) -> Result<usize, String> {
    struct Counter {
        used: usize,
        maximum: usize,
    }
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.used) {
                return Err(io::Error::other(
                    "cargo-audit serialized plan byte limit exceeded",
                ));
            }
            self.used += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { used: 0, maximum };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| "cargo-audit serialized plan byte limit exceeded".to_owned())?;
    Ok(counter.used)
}
fn admit(selected: &[&str]) -> Result<(), String> {
    if selected.len() > MAX_INPUTS {
        return Err("cargo-audit input count exceeds 100000".into());
    }
    let mut bytes = 0usize;
    for path in selected {
        if path.is_empty()
            || path.len() > MAX_PATH
            || path.starts_with('/')
            || path.contains('\0')
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("cargo-audit input must be a bounded canonical relative path".into());
        }
        if path.split('/').count() > MAX_DEPTH {
            return Err("cargo-audit input depth exceeds 128".into());
        }
        bytes = bytes
            .checked_add(path.len())
            .and_then(|n| n.checked_add(1))
            .ok_or("cargo-audit input byte overflow")?;
        if bytes > MAX_BYTES {
            return Err("cargo-audit input paths exceed 8 MiB".into());
        }
    }
    Ok(())
}
fn open_options(directory: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(
        libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if directory { libc::O_DIRECTORY } else { 0 },
    );
    options
}
fn regular(dir: &Dir, name: &str, budget: &mut Budget) -> Result<(), String> {
    budget.probe()?;
    let metadata = dir
        .symlink_metadata(name)
        .map_err(|e| io_error("inspect regular input/lockfile", &e))?;
    if !metadata.is_file() {
        return Err(
            "cargo-audit input/lockfile must be a regular file, not a symlink or special file"
                .into(),
        );
    }
    budget.probe()?;
    let file = dir
        .open_with(name, &open_options(false))
        .map_err(|e| io_error("open regular input/lockfile", &e))?;
    budget.probe()?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error("inspect regular input/lockfile", &e))?;
    if !metadata.is_file() {
        return Err("cargo-audit input/lockfile must be a regular file".into());
    }
    Ok(())
}
fn owner(root: &Dir, selected: &str, budget: &mut Budget) -> Result<String, String> {
    let parts: Vec<&str> = selected.split('/').collect();
    budget.probe()?;
    let mut dirs = vec![root
        .try_clone()
        .map_err(|e| io_error("clone pinned root", &e))?];
    for part in &parts[..parts.len() - 1] {
        budget.probe()?;
        let child = dirs
            .last()
            .expect("root directory present")
            .open_with(part, &open_options(true))
            .map_err(|e| io_error("open input parent", &e))?;
        dirs.push(Dir::from_std_file(child.into_std()));
    }
    regular(
        dirs.last().expect("input parent present"),
        parts[parts.len() - 1],
        budget,
    )?;
    if parts.last() == Some(&"Cargo.lock") {
        return Ok(selected.to_owned());
    }
    for (depth, dir) in dirs.iter().enumerate().rev() {
        budget.probe()?;
        match dir.symlink_metadata("Cargo.lock") {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error("inspect owning lockfile", &error)),
            Ok(metadata) if !metadata.is_file() => return Err(
                "cargo-audit owning lockfile must be a regular file, not a symlink or directory"
                    .into(),
            ),
            Ok(_) => regular(dir, "Cargo.lock", budget)?,
        }
        let prefix = parts[..depth].join("/");
        let lock = if prefix.is_empty() {
            "Cargo.lock".to_owned()
        } else {
            format!("{prefix}/Cargo.lock")
        };
        if lock.len() > MAX_PATH {
            return Err("cargo-audit owning lockfile path exceeds 4096 bytes".into());
        }
        return Ok(lock);
    }
    Err("cargo-audit selected input has no owning Cargo.lock".into())
}
fn io_error(action: &str, error: &io::Error) -> String {
    // Do not include untrusted path bytes or arbitrary underlying error messages.
    format!(
        "cargo-audit {action}: kind={:?} errno={:?}",
        error.kind(),
        error.raw_os_error()
    )
}

#[cfg(test)]
#[path = "candidate_cargo_audit_tests.rs"]
mod tests;
