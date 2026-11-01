//! Explicit runtime handoff keeps Git identity out of reusable test artifacts.
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::OnceLock;

pub fn resolve(value: Option<OsString>) -> Result<String, String> {
    let value =
        value.ok_or("FORGE_TEST_BIN is required; build Forge and pass its absolute path")?;
    let value = value
        .into_string()
        .map_err(|_| "FORGE_TEST_BIN must be UTF-8")?;
    let path = Path::new(&value);
    if !path.is_absolute() {
        return Err("FORGE_TEST_BIN must be absolute".into());
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect FORGE_TEST_BIN: {error}"))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("FORGE_TEST_BIN must name a regular executable, not a symlink".into());
    }
    Ok(value)
}

pub fn path() -> &'static str {
    static BINARY: OnceLock<String> = OnceLock::new();
    BINARY.get_or_init(|| {
        resolve(std::env::var_os("FORGE_TEST_BIN")).unwrap_or_else(|error| panic!("{error}"))
    })
}
