//! Compatibility-only ambient Rust toolchain recovery for the legacy cargo-lint seam.
//! Native candidate audit execution MUST use explicit operator inputs, not this helper.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::{ffi::OsStringExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_FIELDS: usize = 256;
const MAX_ENV_BYTES: usize = 256 * 1024;
const MAX_VALUE: usize = 8192;
const MAX_PATH: usize = 4096;
const MAX_SETTINGS: usize = 64 * 1024;
const MAX_PASSWD: usize = 64 * 1024;

/// Explicit values win even when not usable. Both recoveries are independent.
/// HOME/login paths are compatibility discovery, not newly granted capabilities.
pub fn resolve(
    environment: &BTreeMap<String, String>,
    login_home: Option<&Path>,
) -> Result<BTreeMap<String, String>, String> {
    admit(environment)?;
    if let Some(home) = login_home {
        path_bound(home)?;
    }
    let mut result = environment.clone();
    for (key, directory, rustup) in [
        ("RUSTUP_HOME", ".rustup", true),
        ("CARGO_HOME", ".cargo", false),
    ] {
        if environment.get(key).is_some_and(|v| !v.is_empty()) {
            continue;
        }
        let home = environment
            .get("HOME")
            .filter(|v| !v.is_empty())
            .map(Path::new);
        for candidate in [home, login_home].into_iter().flatten() {
            let candidate = candidate.join(directory);
            path_bound(&candidate)?;
            let usable = if rustup {
                usable_rustup(&candidate)?
            } else {
                usable_cargo(&candidate)?
            };
            if usable {
                let value = candidate
                    .to_str()
                    .ok_or("recovered toolchain path must be UTF-8")?;
                result.insert(key.to_owned(), value.to_owned());
                break;
            }
        }
    }
    admit(&result)?;
    Ok(result)
}
/// Uses the invoking UID's native passwd record; never reads process environment.
pub fn resolve_current(
    environment: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let home_only = resolve(environment, None)?;
    if ["RUSTUP_HOME", "CARGO_HOME"]
        .iter()
        .all(|key| home_only.get(*key).is_some_and(|v| !v.is_empty()))
    {
        return Ok(home_only);
    }
    let home = login_home()?;
    resolve(&home_only, home.as_deref())
}
/// JSON operation cargo-toolchain-env. Login-home overrides are deliberately absent.
pub fn evaluate(payload: &Value) -> Result<Value, String> {
    let object = payload
        .as_object()
        .ok_or("cargo-toolchain-env payload must be an object")?;
    if object.len() != 1 || !object.contains_key("environment") {
        return Err("cargo-toolchain-env requires only environment".into());
    }
    let values = object["environment"]
        .as_object()
        .ok_or("environment must be an object")?;
    if values.len() > MAX_FIELDS {
        return Err("toolchain environment exceeds 256 fields".into());
    }
    let mut environment = BTreeMap::new();
    let mut bytes = 0;
    for (key, value) in values {
        let value = value.as_str().ok_or("environment values must be strings")?;
        admit_entry(key, value, &mut bytes)?;
        environment.insert(key.clone(), value.to_owned());
    }
    serde_json::to_value(resolve_current(&environment)?)
        .map_err(|_| "encode recovered environment".into())
}
fn admit(environment: &BTreeMap<String, String>) -> Result<(), String> {
    if environment.len() > MAX_FIELDS {
        return Err("toolchain environment exceeds 256 fields".into());
    }
    let mut bytes = 0;
    for (key, value) in environment {
        admit_entry(key, value, &mut bytes)?;
    }
    Ok(())
}
fn admit_entry(key: &str, value: &str, bytes: &mut usize) -> Result<(), String> {
    if key.is_empty()
        || key.len() > 128
        || key.contains(['\0', '='])
        || value.len() > MAX_VALUE
        || value.contains('\0')
    {
        return Err("toolchain environment entry exceeds bounds or is malformed".into());
    }
    *bytes = bytes
        .checked_add(key.len())
        .and_then(|n| n.checked_add(value.len()))
        .ok_or("toolchain environment size overflow")?;
    if *bytes > MAX_ENV_BYTES {
        return Err("toolchain environment exceeds 256 KiB".into());
    }
    Ok(())
}
fn path_bound(path: &Path) -> Result<(), String> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() > MAX_PATH || bytes.contains(&0) {
        return Err("toolchain discovery path exceeds bounds".into());
    }
    Ok(())
}
fn unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}
fn error(action: &str, error: &io::Error) -> String {
    format!(
        "toolchain {action}: kind={:?} errno={:?}",
        error.kind(),
        error.raw_os_error()
    )
}
fn usable_cargo(home: &Path) -> Result<bool, String> {
    match fs::metadata(home.join("bin")) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if unavailable(&e) => Ok(false),
        Err(e) => Err(error("cargo metadata", &e)),
    }
}
fn usable_rustup(home: &Path) -> Result<bool, String> {
    let settings = home.join("settings.toml");
    let meta = match fs::metadata(&settings) {
        Ok(meta) => meta,
        Err(e) if unavailable(&e) => return Ok(false),
        Err(e) => return Err(error("rustup metadata", &e)),
    };
    if !meta.is_file() {
        return Err("rustup settings must be a regular file".into());
    }
    if meta.len() > MAX_SETTINGS as u64 {
        return Err("rustup settings exceeds 64 KiB".into());
    }
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&settings)
    {
        Ok(file) => file,
        Err(e) if unavailable(&e) => return Ok(false),
        Err(e) => return Err(error("open rustup settings", &e)),
    };
    let fdmeta = match file.metadata() {
        Ok(meta) => meta,
        Err(e) if unavailable(&e) => return Ok(false),
        Err(e) => return Err(error("inspect rustup settings", &e)),
    };
    if !fdmeta.is_file() {
        return Err("rustup settings must be a regular file".into());
    }
    let mut bytes = Vec::with_capacity((fdmeta.len() as usize).min(MAX_SETTINGS));
    if let Err(e) = file.take(MAX_SETTINGS as u64 + 1).read_to_end(&mut bytes) {
        if unavailable(&e) {
            return Ok(false);
        }
        return Err(error("read rustup settings", &e));
    }
    if bytes.len() > MAX_SETTINGS {
        return Err("rustup settings exceeds 64 KiB".into());
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(false);
    };
    if !text.contains("default_toolchain") {
        return Ok(false);
    }
    let mut entries = match fs::read_dir(home.join("toolchains")) {
        Ok(entries) => entries,
        Err(e) if unavailable(&e) => return Ok(false),
        Err(e) => return Err(error("list toolchains", &e)),
    };
    match entries.next() {
        None => Ok(false),
        Some(Ok(_)) => Ok(true),
        Some(Err(e)) if unavailable(&e) => Ok(false),
        Some(Err(e)) => Err(error("inspect toolchains", &e)),
    }
}
fn login_home() -> Result<Option<PathBuf>, String> {
    let mut size = 4096;
    loop {
        let mut storage = vec![0u8; size];
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                record.as_mut_ptr(),
                storage.as_mut_ptr().cast(),
                storage.len(),
                &mut found,
            )
        };
        if status == libc::ERANGE && size < MAX_PASSWD {
            size = (size * 2).min(MAX_PASSWD);
            continue;
        }
        if matches!(status, libc::ENOENT | libc::ESRCH | libc::EACCES) {
            return Ok(None);
        }
        if status != 0 {
            return Err(format!("passwd lookup failed errno={status}"));
        }
        if found.is_null() {
            return Ok(None);
        }
        let record = unsafe { record.assume_init() };
        return passwd_path(&storage, record.pw_dir);
    }
}
fn passwd_path(storage: &[u8], directory: *const libc::c_char) -> Result<Option<PathBuf>, String> {
    let offset = (directory as usize)
        .checked_sub(storage.as_ptr() as usize)
        .ok_or("passwd directory outside buffer")?;
    let bytes = storage
        .get(offset..)
        .ok_or("passwd directory outside buffer")?;
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or("passwd directory missing terminator")?;
    if end == 0 {
        return Ok(None);
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes[..end].to_vec()));
    path_bound(&path)?;
    Ok(Some(path))
}
#[cfg(test)]
#[path = "candidate_cargo_toolchain_tests.rs"]
mod tests;
