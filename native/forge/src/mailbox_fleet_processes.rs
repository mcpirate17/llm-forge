//! Optional same-user /proc observations; no process is signalled or launched.
use anyhow::{ensure, Context, Result};
use regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) struct Observation {
    pub(super) groups: Map<String, Value>,
    pub(super) coverage: Value,
}

#[derive(Default)]
struct Counts {
    disappeared: usize,
    permission_denied: usize,
    truncated_cmdlines: usize,
    included: usize,
    omitted: usize,
    bytes: usize,
}

impl Counts {
    fn skip_error(&mut self, error: &std::io::Error) -> bool {
        match error.kind() {
            std::io::ErrorKind::NotFound => self.disappeared += 1,
            std::io::ErrorKind::PermissionDenied => self.permission_denied += 1,
            _ => return false,
        }
        true
    }
    fn coverage(&self) -> Value {
        json!({"scope":"bounded, transient same-user process observations; matching cwd or configured command patterns",
            "complete": self.disappeared == 0 && self.permission_denied == 0
                && self.truncated_cmdlines == 0 && self.omitted == 0,
            "disappeared":self.disappeared, "permission_denied":self.permission_denied,
            "truncated_cmdlines":self.truncated_cmdlines,
            "included_rows":self.included, "omitted_rows":self.omitted})
    }
}

pub(super) fn capture(host: &Path) -> Result<Observation> {
    let patterns =
        conductor_native::project_paths::worktree_patterns(host).map_err(anyhow::Error::msg)?;
    ensure!(
        patterns.len() <= 32 && patterns.iter().all(|p| p.len() <= 4096),
        "fleet process patterns exceed bounds"
    );
    let patterns = patterns
        .into_iter()
        .map(|pattern| RegexBuilder::new(&pattern).size_limit(1024 * 1024).build())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    capture_from(Path::new("/proc"), host, &patterns, 1024, 512)
}

fn inspect(
    entry: &fs::DirEntry,
    pid: u32,
    host: &Path,
    patterns: &[Regex],
    counts: &mut Counts,
) -> Result<Option<(String, String)>> {
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) if counts.skip_error(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Ok(None);
    }
    let path = entry.path().join("cmdline");
    let mut bytes = Vec::new();
    match fs::File::open(&path).and_then(|f| f.take(4097).read_to_end(&mut bytes)) {
        Ok(_) => (),
        Err(error) if counts.skip_error(&error) => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    }
    counts.bytes += bytes.len();
    ensure!(
        counts.bytes <= 32 * 1024 * 1024,
        "fleet command inventory exceeds 32 MiB"
    );
    let truncated = bytes.len() > 4096;
    if truncated {
        counts.truncated_cmdlines += 1;
    }
    bytes.truncate(4096);
    let command = String::from_utf8_lossy(&bytes).replace('\0', " ");
    let cwd = match fs::read_link(entry.path().join("cwd")) {
        Ok(cwd) => Some(cwd),
        Err(error) if counts.skip_error(&error) => None,
        Err(error) => return Err(error.into()),
    };
    let group = if cwd.as_ref().is_some_and(|cwd| cwd.starts_with(host)) {
        Some(host.to_string_lossy().into_owned())
    } else {
        patterns.iter().find_map(|pattern| {
            pattern
                .find(&command)
                .map(|found| found.as_str().to_owned())
        })
    };
    Ok(group.map(|group| {
        (
            group,
            format!(
                "{pid} {}{}",
                command.chars().take(140).collect::<String>(),
                if truncated {
                    " [cmdline truncated]"
                } else {
                    ""
                }
            ),
        )
    }))
}

fn capture_from(
    proc_root: &Path,
    host: &Path,
    patterns: &[Regex],
    max_rows: usize,
    max_groups: usize,
) -> Result<Observation> {
    let mut groups: Map<String, Value> = Map::new();
    let mut counts = Counts::default();
    for (count, entry) in fs::read_dir(proc_root)?.enumerate() {
        ensure!(
            count < 65_536,
            "fleet process inventory exceeds 65536 entries"
        );
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if let Some((group, line)) = inspect(&entry, pid, host, patterns, &mut counts)? {
            if counts.included >= max_rows
                || (!groups.contains_key(&group) && groups.len() >= max_groups)
            {
                counts.omitted += 1;
                continue;
            }
            groups
                .entry(group)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .context("process group is not an array")?
                .push(Value::String(line));
            counts.included += 1;
        }
    }
    for value in groups.values_mut() {
        value
            .as_array_mut()
            .context("process group is not an array")?
            .sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    }
    Ok(Observation {
        groups,
        coverage: counts.coverage(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir().join(forge::resource_id("fleet-proc-test").unwrap());
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn process(&self, pid: u32, command: &[u8], cwd: &Path) {
            let path = self.0.join(pid.to_string());
            fs::create_dir(&path).unwrap();
            fs::write(path.join("cmdline"), command).unwrap();
            symlink(cwd, path.join("cwd")).unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn truncated_unmatched_command_and_cwd_match_are_reported() {
        let proc = Scratch::new();
        let host = proc.0.join("host");
        let hidden = format!("{} /target/work", "x".repeat(4096));
        proc.process(123456, hidden.as_bytes(), Path::new("/elsewhere"));
        proc.process(123457, b"agent\0run", &host.join("child"));
        let observation = capture_from(
            &proc.0,
            &host,
            &[Regex::new("/target/work").unwrap()],
            10,
            10,
        )
        .unwrap();
        assert_eq!(
            observation.groups[host.to_str().unwrap()]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(observation.coverage["truncated_cmdlines"], 1);
        assert_eq!(observation.coverage["complete"], false);
    }

    #[test]
    fn row_and_group_caps_count_omissions() {
        let proc = Scratch::new();
        for (pid, name) in [
            (123456, "/work/a"),
            (123457, "/work/b"),
            (123458, "/work/c"),
        ] {
            proc.process(pid, name.as_bytes(), Path::new("/elsewhere"));
        }
        let patterns = [Regex::new("/work/[abc]").unwrap()];
        let rows = capture_from(&proc.0, Path::new("/host"), &patterns, 1, 10).unwrap();
        assert_eq!(rows.coverage["included_rows"], 1);
        assert_eq!(rows.coverage["omitted_rows"], 2);
        let groups = capture_from(&proc.0, Path::new("/host"), &patterns, 10, 1).unwrap();
        assert_eq!(groups.coverage["included_rows"], 1);
        assert_eq!(groups.coverage["omitted_rows"], 2);
        assert_eq!(groups.coverage["complete"], false);
    }
}
