//! Port of `tooling/hooks/claude/obsidian_sync.py`'s **post-edit path only**
//! (`cmd_post_edit`): the SessionEnd half (`cmd_session_end`) stays in
//! Python -- it reads the accumulator this path writes, sums a session, and
//! has no latency budget to justify a port.
//!
//! PostToolUse on Edit/Write: if the edited file is a memory entry under the
//! session's memory root, mirror it into the Obsidian vault as a thin
//! backlink note (the repo memory file stays the source of truth), and
//! always append one accumulator line (`<utc-iso>\t<kind>\t<path>`) so
//! session-end can summarize. The hook's own answer is always the quiet ok
//! JSON the body prints; a broken vault or accumulator degrades silently to
//! stderr-free best effort, exactly like the Python body's bare `except`.
//!
//! Path resolutions port verbatim (`_env_dir` honours `~`, a relative value
//! resolves against the checkout). One divergence, documented: Python's
//! `repo_root()` falls back to the module file's own location when both
//! `CLAUDE_PROJECT_DIR` and `PROJECT_DIR` are unset; forge has no module
//! file, so the fallback here is the session checkout
//! (`interpreter::project_root()`, the cwd the dispatcher runs from) -- the
//! same authority every other native hook already uses.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::instant;

/// `repo_root()`: `CLAUDE_PROJECT_DIR`, then `PROJECT_DIR`, then the session
/// checkout (see module docs for the one-file divergence).
pub fn repo_root() -> PathBuf {
    for name in ["CLAUDE_PROJECT_DIR", "PROJECT_DIR"] {
        if let Ok(raw) = std::env::var(name) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
    }
    crate::interpreter::project_root()
}

/// `_env_dir`: `$name` as a directory (`~` expanded, a relative value
/// against *root*), else *default*.
fn env_dir(name: &str, default: PathBuf, root: &Path) -> PathBuf {
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return default;
    }
    let path = expand_home(trimmed);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

/// `Path.expanduser()`: only the leading `~`, as Python resolves it.
fn expand_home(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `_memory_slug`: the per-project memory dir name for *root* -- every `/`,
/// `_` and `.` in the absolute path becomes `-` (Claude Code's own slug
/// rule, mirrored character-for-character).
fn memory_slug(root: &Path) -> String {
    root.display().to_string().replace(['/', '_', '.'], "-")
}

/// `_VAULT`/`VAULT_ROOT`: `OBSIDIAN_VAULT_ROOT` (default
/// `~/Documents/CodexVault`), plus the `claude/` child the mirror writes to.
pub fn vault_root(root: &Path) -> PathBuf {
    let vault = env_dir(
        "OBSIDIAN_VAULT_ROOT",
        home_dir().join("Documents").join("CodexVault"),
        root,
    );
    vault.join("claude")
}

/// `MEMORY_ROOT`: `CLAUDE_MEMORY_ROOT`, default
/// `~/.claude/projects/<memory_slug(root)>/memory`.
pub fn memory_root(root: &Path) -> PathBuf {
    let slug = memory_slug(root);
    env_dir(
        "CLAUDE_MEMORY_ROOT",
        home_dir()
            .join(".claude")
            .join("projects")
            .join(slug)
            .join("memory"),
        root,
    )
}

/// `_slug`: lower-case, every run of non `[a-z0-9_-]` collapsed to `-`,
/// edge-dashed stripped, empty becomes `untitled`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-' {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch);
        } else {
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed
    }
}

/// `FM_LINE_RE`: `key: value` frontmatter lines.
fn frontmatter_field(line: &str) -> Option<(&str, &str)> {
    let mut split = line.splitn(2, ':');
    let key = split.next()?.trim();
    if key.is_empty()
        || !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let value = split.next()?.trim();
    Some((key, value))
}

/// `_parse_frontmatter`: the `---`-delimited header's `key: value` pairs
/// (quotes stripped) and the body after it.
fn parse_frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    if !text.starts_with("---\n") {
        return (Vec::new(), text.to_string());
    }
    let Some(end) = text[4..].find("\n---").map(|i| i + 4) else {
        return (Vec::new(), text.to_string());
    };
    let mut fields = Vec::new();
    for line in text[4..end].lines() {
        if let Some((key, value)) = frontmatter_field(line) {
            let stripped = value.trim_matches('"').trim_matches('\'');
            fields.push((key.to_string(), stripped.to_string()));
        }
    }
    let body = text[end + 4..].trim_start_matches('\n').to_string();
    (fields, body)
}

/// `_mirror_memory`: one thin backlink note under `VAULT_ROOT/memory/`,
/// dated today, pointing at the canonical memory file. Best effort like the
/// Python body: a failure is the caller's `except Exception: pass`.
fn mirror_memory(memory_path: &Path, vault: &Path, today: &str) -> std::io::Result<()> {
    let text = fs::read_to_string(memory_path)?;
    let (fields, body) = parse_frontmatter(&text);
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let name = field("name").unwrap_or_else(|| {
        memory_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let description = field("description").unwrap_or_default();
    let mtype = field("type").unwrap_or_else(|| "memory".to_string());

    let out_dir = vault.join("memory");
    fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!(
        "{}.md",
        slug(
            &memory_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )
    ));
    let canonical = memory_path.display().to_string();
    let mut parts: Vec<String> = vec![
        "---".to_string(),
        format!("date: {today}"),
        "source: claude-code-memory".to_string(),
        format!("type: {mtype}"),
        format!("canonical: \"{canonical}\""),
        format!("tags: [project/llm, source/claude-code, memory/{mtype}]"),
        "---".to_string(),
        String::new(),
        format!("# {name}"),
        String::new(),
        format!("> **Canonical** (auto-memory): `{canonical}`"),
        "> Thin mirror — the repo memory file is the source of truth.".to_string(),
        String::new(),
    ];
    if !description.is_empty() {
        parts.push(format!("**{description}**"));
        parts.push(String::new());
    }
    if !body.trim().is_empty() {
        parts.push(body.trim_end().to_string());
        parts.push(String::new());
    }
    fs::write(&out_path, parts.join("\n"))
}

/// `_is_memory_file`: under the memory root (a proper ancestor) and `.md`.
fn is_memory_file(path: &Path, memory_root: &Path) -> bool {
    path != memory_root
        && path.starts_with(memory_root)
        && path.extension().is_some_and(|ext| ext == "md")
}

/// `cmd_post_edit`: the whole post-edit path -- classify the file, mirror a
/// memory entry, append the accumulator line, answer ok. Returns the quiet
/// JSON the body prints (`_emit_ok`).
pub fn post_edit_output(payload: &Value) -> Value {
    let tool_input = payload.get("tool_input").filter(|value| value.is_object());
    let fp_raw = tool_input
        .and_then(|input| input.get("file_path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let sid = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    if fp_raw.is_empty() {
        return quiet_ok();
    }

    let root = repo_root();
    let mem_root = memory_root(&root);
    let fp = PathBuf::from(fp_raw);
    let mut kind = "edit";
    if is_memory_file(&fp, &mem_root) {
        if fp.file_name().is_some_and(|n| n == "MEMORY.md") {
            kind = "memory-index";
        } else {
            kind = "memory";
            // `datetime.now().strftime("%Y-%m-%d")`: the first ten chars of
            // the ISO stamp are exactly that date.
            let today = instant::isoformat_utc(instant::now())[..10].to_string();
            let _ = mirror_memory(&fp, &vault_root(&root), &today);
        }
    }

    let ts = instant::isoformat_utc(instant::now());
    if let Err(err) = append_accumulator(sid, &format!("{ts}\t{kind}\t{fp_raw}\n")) {
        eprintln!("obsidian post-edit: accumulator unavailable: {err}");
    }
    quiet_ok()
}

/// The accumulator append: `/tmp/claude-session-journal/<sid>.tsv`, created
/// on demand. Python swallows the OSError; the caller only wants the line on
/// disk when the disk allows it, so the error returns to be logged.
fn append_accumulator(sid: &str, line: &str) -> std::io::Result<()> {
    let dir = PathBuf::from("/tmp/claude-session-journal");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{sid}.tsv"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    write!(file, "{line}")
}

fn quiet_ok() -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-obsidian-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_memory_slug_and_slug_rules_mirror_python() {
        assert_eq!(
            memory_slug(Path::new("/home/tim/Projects/llm_forge")),
            "-home-tim-Projects-llm-forge"
        );
        assert_eq!(slug("Coffee Habits! (2026)"), "coffee-habits-2026");
        assert_eq!(slug("!!!"), "untitled");
        // A run of disallowed characters collapses to ONE dash, but the
        // allowed dashes inside it survive: `re.sub(r"[^a-z0-9_-]+", "-")`.
        assert_eq!(slug("a  --  b"), "a----b");
    }

    #[test]
    fn frontmatter_parses_like_the_python_regex() {
        let text =
            "---\nname: My Memory\ndescription: \"Quoted\"\ntype: feedback\n---\n\nBody text.\n";
        let (fields, body) = parse_frontmatter(text);
        let get = |k: &str| {
            fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("name"), "My Memory");
        assert_eq!(get("description"), "Quoted");
        assert_eq!(get("type"), "feedback");
        assert_eq!(body, "Body text.\n");
        let (none, raw) = parse_frontmatter("no frontmatter\n");
        assert!(none.is_empty());
        assert_eq!(raw, "no frontmatter\n");
    }

    #[test]
    fn a_memory_file_mirrors_and_a_plain_edit_does_not() {
        let dir = scratch("mirror");
        let mem_root = dir.join("mem");
        let vault = dir.join("vault").join("claude");
        fs::create_dir_all(mem_root.join("sub")).unwrap();
        let memory = mem_root.join("sub").join("tips.md");
        fs::write(
            &memory,
            "---\nname: Tips\ndescription: small tips\ntype: feedback\n---\n\nUse it.\n",
        )
        .unwrap();
        mirror_memory(&memory, &vault, "2026-09-13").unwrap();
        let note = fs::read_to_string(vault.join("memory").join("tips.md")).unwrap();
        assert!(note.starts_with("---\ndate: 2026-09-13\n"));
        assert!(note.contains("canonical: \""), "{note}");
        assert!(note.contains("# Tips"));
        assert!(note.contains("**small tips**"));
        assert!(note.ends_with("Use it.\n"));
        assert!(!note.ends_with("Use it.\n\n")); // one trailing newline, like join
        assert!(is_memory_file(&memory, &mem_root));
        // MEMORY.md under the root is a memory file too -- the name check
        // that makes it "memory-index" happens in `post_edit_output`, after
        // this predicate says yes.
        assert!(is_memory_file(&mem_root.join("MEMORY.md"), &mem_root));
        assert!(!is_memory_file(&dir.join("MEMORY.md"), &mem_root));
        assert!(!is_memory_file(&mem_root, &mem_root));
        fs::remove_dir_all(&dir).ok();
    }
}
