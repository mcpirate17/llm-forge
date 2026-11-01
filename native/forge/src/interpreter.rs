//! Host-root resolution shared by native hooks. The historical module name is
//! retained for path-included native tests; no interpreter resolution remains.
use std::env;
use std::path::PathBuf;

pub fn project_root() -> PathBuf {
    match env::var("CLAUDE_PROJECT_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}
