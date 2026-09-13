//! Native port of `_bash_impact.py`: classifies a proposed Bash command as
//! `allow` or `soft_warn` and, for `soft_warn`, computes a quick impact summary
//! (file count, size, row count) the agent must surface to the user before the
//! command runs.
//!
//! Behaviour mirrors the Python module line for line, with one deliberate fix
//! carried into both implementations at once: sizing a directory used to shell
//! out to `du -sh` (a subprocess per `rm -rf`/`find -delete`/`git clean` target).
//! That shellout is gone here (and in `_bash_impact.py`'s own `_du_summary`, see
//! that file's PR) in favour of summing `st_size`/`metadata().len()` over an
//! in-process walk -- no subprocess, and it makes the two implementations agree
//! exactly instead of one reporting allocated-block sizes and the other apparent
//! byte sizes. The walk uses `walkdir`, not the `ignore` crate: `_bash_impact.py`
//! never consulted `.gitignore` either (`Path.rglob("*")` walks everything), so
//! matching that means *not* filtering by it here.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

/// The two verdicts `_classify` returns (Python's `tier: Literal["allow", "soft_warn"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Allow,
    SoftWarn,
}

static RM_RF_ARGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\brm\s+-[rRf]+\w*\s+([^;|&\n]+)").unwrap());
static SPLIT_TRAILING_OPERATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s(?:&&|\|\||>>?|<<?)\s").unwrap());
static FIND_DELETE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\bfind\s+(\S+).*-delete\b").unwrap());
static GIT_CLEAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgit\s+clean\s+-[fdxX]+\s*([^\s;|&]*)").unwrap());
static SQLITE_MUTATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bsqlite3\s+(\S+)\s+["']?\s*(DELETE|DROP|UPDATE|TRUNCATE)\b([^"']*)"#)
        .unwrap()
});
static SQLITE_DELETE_FROM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)DELETE\s+FROM\s+(\w+)\s*(?:WHERE\s+(.+?))?\s*["';]*$"#).unwrap()
});

/// Mirrors `_bash_impact._human_size`: byte count bucketed into B/K/M/G/T with
/// truncating (not rounding) integer division at each step.
fn human_size(mut n: u64) -> String {
    for unit in ["B", "K", "M", "G"] {
        if n < 1024 {
            return format!("{n}{unit}");
        }
        n /= 1024;
    }
    format!("{n}T")
}

/// Inverse of `human_size`, mirroring `_bash_impact._to_bytes`: best-effort,
/// returns 0 on anything that doesn't parse. Used only to re-total several
/// `rm -rf` targets' already-rounded sizes, exactly as the Python does (the
/// round trip is lossy on purpose -- it matches what the agent actually reads).
fn to_bytes(human: &str) -> u64 {
    let human = human.trim();
    if human.is_empty() {
        return 0;
    }
    let unit_pos =
        human.find(|c: char| matches!(c.to_ascii_uppercase(), 'B' | 'K' | 'M' | 'G' | 'T'));
    let Some(pos) = unit_pos else { return 0 };
    let (number, unit) = human.split_at(pos);
    let Ok(value) = number.parse::<f64>() else {
        return 0;
    };
    let multiplier: u64 = match unit.chars().next().unwrap().to_ascii_uppercase() {
        'B' => 1,
        'K' => 1024,
        'M' => 1024 * 1024,
        'G' => 1024 * 1024 * 1024,
        'T' => 1024 * 1024 * 1024 * 1024,
        _ => return 0,
    };
    (value * multiplier as f64) as u64
}

/// Mirrors `_bash_impact._du_summary`: `(file_count, human_size)`, best-effort,
/// never panics. `-1` file count signals "existed but could not be measured"
/// (Python's blanket `except Exception`), surfaced as `"?"`.
fn du_summary(path: &str) -> (i64, String) {
    let p = Path::new(path);
    let meta = match fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return (0, "0B".to_string()),
    };
    if meta.is_file() {
        return (1, human_size(meta.len()));
    }
    if !meta.is_dir() {
        // Exists but is neither a plain file nor a directory (device, socket,
        // ...): Python's `p.rglob("*")` on that would raise, landing in the
        // blanket `except Exception -> (-1, "?")`.
        return (-1, "?".to_string());
    }
    let mut files: i64 = 0;
    let mut total: u64 = 0;
    for entry in walkdir::WalkDir::new(p).follow_links(false).min_depth(1) {
        let Ok(entry) = entry else { continue };
        // Follow symlinks the same way `Path.is_file()`/`Path.stat()` do: a
        // symlink to a file counts (with the target's size); a symlink to a
        // directory is neither descended into (`walkdir` already doesn't,
        // matching `rglob`'s refusal to follow directory symlinks) nor counted.
        if let Ok(target_meta) = fs::metadata(entry.path()) {
            if target_meta.is_file() {
                files += 1;
                total += target_meta.len();
            }
        }
    }
    (files, human_size(total))
}

fn sql_row_count(db_path: &str, table: &str, where_clause: Option<&str>) -> Option<i64> {
    if !Path::new(db_path).exists() {
        return None;
    }
    let mut sql = format!("SELECT COUNT(*) FROM {table}");
    if let Some(where_clause) = where_clause {
        sql.push_str(" WHERE ");
        sql.push_str(where_clause);
    }
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg(&sql)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i64>()
        .ok()
}

fn strip_quotes(token: &str) -> &str {
    token.trim_matches(|c| c == '"' || c == '\'')
}

/// Mirrors `_bash_impact._classify`: `(tier, impact_text)`. `impact_text` is
/// only meaningful (non-empty) for `Tier::SoftWarn`.
pub fn classify(cmd: &str) -> (Tier, String) {
    let mut impacts: Vec<String> = Vec::new();

    let mut rm_total_files: i64 = 0;
    let mut rm_total_bytes: u64 = 0;
    let mut rm_lines: Vec<String> = Vec::new();
    for caps in RM_RF_ARGS.captures_iter(cmd) {
        let args_blob = &caps[1];
        let args_blob = SPLIT_TRAILING_OPERATOR
            .splitn(args_blob, 2)
            .next()
            .unwrap_or(args_blob);
        for token in args_blob.split_whitespace() {
            let target = strip_quotes(token);
            if target.is_empty() || target.starts_with('-') {
                continue;
            }
            let (files, size) = du_summary(target);
            if files <= 0 {
                continue;
            }
            rm_total_files += files;
            rm_total_bytes += to_bytes(&size);
            rm_lines.push(format!("rm -rf {target}: {files} files, {size}"));
        }
    }
    if !rm_lines.is_empty() {
        if rm_lines.len() > 1 {
            rm_lines.push(format!(
                "TOTAL: {rm_total_files} files, {}",
                human_size(rm_total_bytes)
            ));
        }
        impacts.extend(rm_lines);
    }

    for caps in FIND_DELETE.captures_iter(cmd) {
        let target = strip_quotes(&caps[1]);
        let (files, size) = du_summary(target);
        if files > 0 {
            impacts.push(format!(
                "find -delete in {target}: up to {files} files, {size}"
            ));
        }
    }

    for caps in GIT_CLEAN.captures_iter(cmd) {
        let raw_target = strip_quotes(&caps[1]);
        let target = if raw_target.is_empty() {
            "."
        } else {
            raw_target
        };
        let (files, size) = du_summary(target);
        if files > 0 {
            impacts.push(format!(
                "git clean in {target}: scope ~{files} files, {size}"
            ));
        }
    }

    for caps in SQLITE_MUTATE.captures_iter(cmd) {
        let db_path = strip_quotes(&caps[1]);
        let verb = caps[2].to_uppercase();
        let rest = caps.get(3).map_or("", |m| m.as_str());
        let mut row_msg = String::new();
        if verb == "DELETE" {
            let reconstructed = format!("{verb}{rest}");
            if let Some(from_caps) = SQLITE_DELETE_FROM.captures(&reconstructed) {
                let table = &from_caps[1];
                let where_clause = from_caps
                    .get(2)
                    .map(|m| m.as_str().trim())
                    .map(|w| w.trim_end_matches(|c| ";\"' ".contains(c)))
                    .filter(|w| !w.is_empty());
                if let Some(count) = sql_row_count(db_path, table, where_clause) {
                    row_msg = format!(" \u{2192} {count} rows");
                }
            }
        }
        impacts.push(format!("sqlite3 {db_path} {verb}{rest}{row_msg}"));
    }

    if !impacts.is_empty() {
        return (
            Tier::SoftWarn,
            format!("\n  \u{2022} {}", impacts.join("\n  \u{2022} ")),
        );
    }
    (Tier::Allow, String::new())
}

/// Mirrors `_bash_impact.main`'s allow-path formatting: the exact
/// `additionalContext` string surfaced to the agent for a `soft_warn` verdict,
/// or `None` for a plain allow (Python's bare `_emit("allow")`, no context key).
pub fn additional_context(cmd: &str) -> Option<String> {
    let (tier, impacts) = classify(cmd);
    if tier == Tier::Allow {
        return None;
    }
    Some(format!(
        "DESTRUCTIVE COMMAND IMPACT (must summarize to user before/after running):{impacts}\n\
         If the user did not explicitly authorize this scope, stop and ask. Do not run \
         cleanups for impact >100 files / >100 MB / >1000 rows without explicit \
         confirmation in the current turn."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_matches_python_bucketing() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(1023), "1023B");
        assert_eq!(human_size(1024), "1K");
        assert_eq!(human_size(4096), "4K");
        assert_eq!(human_size(1024 * 1024), "1M");
    }

    #[test]
    fn to_bytes_round_trips_human_size() {
        assert_eq!(to_bytes("4K"), 4096);
        assert_eq!(to_bytes("0B"), 0);
        assert_eq!(to_bytes("?"), 0);
        assert_eq!(to_bytes(""), 0);
    }

    #[test]
    fn allow_when_nothing_destructive_matches() {
        let (tier, _) = classify("echo hi");
        assert_eq!(tier, Tier::Allow);
        assert!(additional_context("echo hi").is_none());
    }

    #[test]
    fn allow_when_rm_target_does_not_exist() {
        let (tier, _) = classify("rm -rf /no/such/path/at/all-forge-test");
        assert_eq!(tier, Tier::Allow);
    }

    #[test]
    fn soft_warn_on_rm_rf_of_a_real_file() {
        let dir = std::env::temp_dir().join(format!("forge-impact-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("victim.txt");
        std::fs::write(&file, b"x".repeat(4096)).unwrap();
        let cmd = format!("rm -rf {}", file.display());
        let (tier, text) = classify(&cmd);
        assert_eq!(tier, Tier::SoftWarn);
        assert!(text.contains("1 files, 4K"), "{text}");
        let context = additional_context(&cmd).expect("soft_warn has context");
        assert!(context.starts_with("DESTRUCTIVE COMMAND IMPACT"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn du_summary_does_not_descend_into_symlinked_directories() {
        let base =
            std::env::temp_dir().join(format!("forge-impact-symlink-{}", std::process::id()));
        let inside = base.join("inside");
        let outside =
            std::env::temp_dir().join(format!("forge-impact-outside-{}", std::process::id()));
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("c.txt"), b"z".repeat(4096)).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, inside.join("link_to_dir")).unwrap();
        let (files, _) = du_summary(inside.to_str().unwrap());
        assert_eq!(
            files, 0,
            "must not count files behind a symlinked directory"
        );
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }
}
