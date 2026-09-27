//! Read-only graph edges and bounded syntactic caller fallback.

use super::{GraphRelationship, GraphResult};
use regex::Regex;
use rusqlite::{params_from_iter, types::Value, Connection, OpenFlags};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const RG_OUTPUT_CAP: usize = 2_000_000;
const SCAN_BYTE_CAP: usize = 64_000_000;
const SCAN_ENTRY_CAP: usize = 100_000;
const SYNTACTIC_HITS_CAP: usize = 10_000;
const RG_TIMEOUT: Duration = Duration::from_secs(5);

fn normalized(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => parts.push("..".to_owned()),
            Component::RootDir => parts.push(String::new()),
            Component::Prefix(part) => parts.push(part.as_os_str().to_string_lossy().into_owned()),
        }
    }
    if parts.first().is_some_and(String::is_empty) {
        format!("/{}", parts[1..].join("/"))
    } else {
        parts.join("/")
    }
}

fn absolute_target(repo: &Path, file_path: &str) -> String {
    let joined = repo.join(file_path);
    if let Ok(canonical) = joined.canonicalize() {
        return canonical.to_string_lossy().into_owned();
    }
    // `Path.resolve()` in the Python entry point also resolves an existing
    // parent when the requested source has not yet been materialized.
    let mut ancestor = joined.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        if let Some(name) = ancestor.file_name() {
            missing.push(name.to_os_string());
        }
        let Some(parent) = ancestor.parent() else {
            break;
        };
        ancestor = parent;
    }
    let mut result = ancestor
        .canonicalize()
        .unwrap_or_else(|_| ancestor.to_path_buf());
    for part in missing.into_iter().rev() {
        result.push(part);
    }
    result.to_string_lossy().into_owned()
}

fn rel_path(repo: &Path, path: &str) -> String {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return path.to_owned();
    }
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    candidate
        .strip_prefix(root)
        .map_or_else(|_| path.to_owned(), normalized)
}

fn rows(
    conn: &Connection,
    repo: &Path,
    absolute: &str,
    relative: &str,
    target: Option<&str>,
    inbound: bool,
) -> rusqlite::Result<Vec<GraphRelationship>> {
    let (focus, other, target_field) = if inbound {
        ("target", "source", "target")
    } else {
        ("source", "target", "source")
    };
    let condition = if target.is_some() {
        format!("AND {target_field}.name = ?3")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT DISTINCT {other}.qualified_name, edge.kind, {other}.file_path \
         FROM nodes AS {focus} \
         JOIN edges AS edge ON edge.{focus}_qualified = {focus}.qualified_name \
         JOIN nodes AS {other} ON {other}.qualified_name = edge.{other}_qualified \
         WHERE {focus}.file_path IN (?1, ?2) {condition} \
         AND edge.kind != 'contains' ORDER BY {other}.qualified_name LIMIT 50"
    );
    let mut params = vec![
        Value::Text(absolute.to_owned()),
        Value::Text(relative.to_owned()),
    ];
    if let Some(target_name) = target {
        params.push(Value::Text(target_name.to_owned()));
    }
    let mut statement = conn.prepare(&sql)?;
    let found = statement.query_map(params_from_iter(params.iter()), |row| {
        Ok(GraphRelationship {
            qualified_name: rel_path(repo, &row.get::<_, String>(0)?),
            kind: row.get(1)?,
            file_path: rel_path(repo, &row.get::<_, Option<String>>(2)?.unwrap_or_default()),
        })
    })?;
    found.collect()
}

fn read_graph(
    repo: &Path,
    db_path: &Path,
    file_path: &str,
    target: Option<&str>,
) -> rusqlite::Result<(Vec<GraphRelationship>, Vec<GraphRelationship>)> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let absolute = absolute_target(repo, file_path);
    let relative = normalized(Path::new(file_path));
    let callers = rows(&conn, repo, &absolute, &relative, target, true)?;
    let callees = rows(&conn, repo, &absolute, &relative, target, false)?;
    Ok((callers, callees))
}

pub(super) fn query(
    repo: &Path,
    file_path: &str,
    target: Option<&str>,
) -> Result<GraphResult, String> {
    let db_path = repo.join(".code-review-graph/graph.db");
    let (mut callers, callees, status) = if db_path.is_file() {
        match read_graph(repo, &db_path, file_path, target) {
            Ok((callers, callees)) => (callers, callees, "ok".to_owned()),
            Err(error) => (
                Vec::new(),
                Vec::new(),
                format!("unavailable (sqlite error: {error})"),
            ),
        }
    } else {
        (
            Vec::new(),
            Vec::new(),
            "unavailable (graph.db missing)".to_owned(),
        )
    };
    if let Some(symbol) = target {
        let mut seen: HashSet<String> = callers.iter().map(|row| row.file_path.clone()).collect();
        for candidate in syntactic_callers(repo, symbol, file_path)? {
            if seen.insert(candidate.file_path.clone()) {
                callers.push(candidate);
            }
        }
    }
    Ok(GraphResult {
        callers,
        callees,
        status,
    })
}

fn stop_group(child: &mut Child) -> Result<(), String> {
    let result = unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error().to_string());
    }
    child.wait().map_err(|error| error.to_string())?;
    Ok(())
}

fn set_nonblocking(descriptor: i32) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

fn capture_rg(child: &mut Child, timeout: Duration, cap: usize) -> Result<Option<Vec<u8>>, String> {
    let stdout = child.stdout.take().ok_or("ripgrep stdout missing")?;
    let descriptor = stdout.as_raw_fd();
    set_nonblocking(descriptor)?;
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut open = true;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        if open {
            let mut poll = libc::pollfd {
                fd: descriptor,
                events: libc::POLLIN,
                revents: 0,
            };
            let milliseconds = remaining.as_millis().clamp(1, 100) as i32;
            let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
            if ready < 0 {
                if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(io::Error::last_os_error().to_string());
            }
            if ready > 0 && poll.revents != 0 {
                let mut chunk = [0_u8; 8192];
                let count =
                    unsafe { libc::read(descriptor, chunk.as_mut_ptr().cast(), chunk.len()) };
                if count == 0 {
                    open = false;
                } else if count > 0 {
                    let count = count as usize;
                    if count > cap.saturating_sub(output.len()) {
                        return Err("ripgrep output exceeds graph context limit".to_owned());
                    }
                    output.extend_from_slice(&chunk[..count]);
                } else if !matches!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::EINTR | libc::EAGAIN)
                ) {
                    return Err(io::Error::last_os_error().to_string());
                }
            }
        }
        if !open
            && child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
        {
            return Ok(Some(output));
        }
        if !open {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn capture_group(
    child: &mut Child,
    timeout: Duration,
    cap: usize,
) -> Result<Option<Vec<u8>>, String> {
    let output = capture_rg(child, timeout, cap);
    if !matches!(&output, Ok(Some(_))) {
        stop_group(child)?;
    }
    output
}

fn isolate_group(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

fn ripgrep_hits(repo: &Path, pattern: &str) -> Result<Option<Vec<(String, usize)>>, String> {
    let mut command = Command::new("rg");
    command
        .args(["-n", "--glob", "*.py", pattern, "."])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Isolate a wrapper and its descendants so timeout or overflow can reap all of them.
    isolate_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("ripgrep failed: {error}")),
    };
    let Some(bytes) = capture_group(&mut child, RG_TIMEOUT, RG_OUTPUT_CAP)? else {
        return Ok(None);
    };
    let mut hits = Vec::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let mut parts = line.splitn(3, ':');
        let Some(path) = parts.next() else { continue };
        let Some(line_number) = parts.next() else {
            continue;
        };
        let line_number = line_number
            .parse::<usize>()
            .map_err(|error| error.to_string())?;
        hits.push((normalized(Path::new(path)), line_number));
        if hits.len() > SYNTACTIC_HITS_CAP {
            return Err("syntactic caller count exceeds graph context limit".to_owned());
        }
    }
    Ok(Some(hits))
}

fn scan_dir(
    repo: &Path,
    dir: &Path,
    pattern: &Regex,
    hits: &mut Vec<(String, usize)>,
    remaining_bytes: &mut usize,
    remaining_entries: &mut usize,
) -> Result<(), String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
        if *remaining_entries == 0 {
            return Err("syntactic scan exceeds graph context entry limit".to_owned());
        }
        entries.push(entry.map_err(|error| error.to_string())?);
        *remaining_entries -= 1;
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let path = entry.path();
        if file_type.is_dir() {
            if !name.starts_with('.') {
                scan_dir(
                    repo,
                    &path,
                    pattern,
                    hits,
                    remaining_bytes,
                    remaining_entries,
                )?;
            }
        } else if file_type.is_file() && name.ends_with(".py") {
            let relative = path.strip_prefix(repo).map_err(|error| error.to_string())?;
            let length = entry.metadata().map_err(|error| error.to_string())?.len();
            if length > *remaining_bytes as u64 {
                return Err("syntactic scan exceeds graph context byte limit".to_owned());
            }
            let mut contents = Vec::new();
            File::open(&path)
                .map_err(|error| error.to_string())?
                .take(*remaining_bytes as u64 + 1)
                .read_to_end(&mut contents)
                .map_err(|error| error.to_string())?;
            if contents.len() > *remaining_bytes {
                return Err("syntactic scan exceeds graph context byte limit".to_owned());
            }
            *remaining_bytes -= contents.len();
            for (index, line) in String::from_utf8_lossy(&contents).lines().enumerate() {
                if pattern.is_match(line) {
                    hits.push((normalized(relative), index + 1));
                    if hits.len() > SYNTACTIC_HITS_CAP {
                        return Err("syntactic caller count exceeds graph context limit".to_owned());
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn syntactic_callers(
    repo: &Path,
    symbol: &str,
    target_file: &str,
) -> Result<Vec<GraphRelationship>, String> {
    let pattern = format!(r"\b{}\(", regex::escape(symbol));
    let regex = Regex::new(&pattern).map_err(|error| error.to_string())?;
    let hits = match ripgrep_hits(repo, &pattern)? {
        Some(hits) => hits,
        None => {
            let mut hits = Vec::new();
            let mut remaining_bytes = SCAN_BYTE_CAP;
            let mut remaining_entries = SCAN_ENTRY_CAP;
            scan_dir(
                repo,
                repo,
                &regex,
                &mut hits,
                &mut remaining_bytes,
                &mut remaining_entries,
            )?;
            hits
        }
    };
    let target = normalized(Path::new(target_file));
    Ok(hits
        .into_iter()
        .filter(|(file, _)| file != &target && !file.starts_with(".venv/"))
        .map(|(file, line)| GraphRelationship {
            qualified_name: format!("{file}:{line}"),
            kind: "calls (syntactic)".to_owned(),
            file_path: file,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{capture_group, isolate_group};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn pid_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "forge-graph-rg-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn shell(script: &str, pid_file: &Path) -> std::process::Child {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                script,
                "graph-context-test",
                pid_file.to_str().unwrap(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        isolate_group(&mut command);
        command.spawn().unwrap()
    }

    #[test]
    fn output_limit_kills_and_reaps_the_producer() {
        let pid_path = pid_file("overflow");
        let mut child = shell(
            "echo $$ > \"$1\"; while :; do printf 'repeat\\n'; done",
            &pid_path,
        );
        let pid = child.id();
        let error = capture_group(&mut child, Duration::from_secs(2), 1024).unwrap_err();
        assert!(error.contains("output exceeds graph context limit"));
        assert_eq!(
            fs::read_to_string(&pid_path).unwrap().trim(),
            pid.to_string()
        );
        assert!(child.try_wait().unwrap().is_some());
        fs::remove_file(pid_path).unwrap();
    }

    #[test]
    fn timeout_reaps_group_even_when_descendant_holds_stdout() {
        let pid_path = pid_file("timeout");
        let mut child = shell("sleep 10 & echo $! > \"$1\"; wait", &pid_path);
        let started = Instant::now();
        let result = capture_group(&mut child, Duration::from_millis(150), 1024).unwrap();
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        let descendant: i32 = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(descendant > 0);
        assert!(child.try_wait().unwrap().is_some());
        fs::remove_file(pid_path).unwrap();
    }
}
