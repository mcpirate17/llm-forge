//! Bounded native libgit2 clean/smudge conversion for identity and EOL.
//! Writers of effective attributes and Git config must honor SyncLease.
use crate::{candidate_source::Source, checkout_sync_worktree::SyncLease};
use anyhow::{bail, ensure, Result};
use git2::{AttrCheckFlags, AttrValue, ErrorCode};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
    ptr,
};

const LIMIT: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Attribute {
    Unspecified,
    True,
    False,
    Value(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    attrs: Vec<Attribute>,
    config: Vec<Option<String>>,
}
impl Policy {
    pub(crate) fn capture(source: &Source, lease: &SyncLease, path: &str) -> Result<Self> {
        crate::candidate_source::safe_path(path.as_bytes())?;
        lease.verify()?;
        let fresh = Source::open(source.root())?;
        ensure!(
            fresh.root() == source.root() && fresh.common_dir() == source.common_dir(),
            "Git source changed during filter admission"
        );
        let repo = fresh.repository();
        let mut attrs = Vec::new();
        for name in [
            "filter",
            "working-tree-encoding",
            "ident",
            "text",
            "eol",
            "crlf",
        ] {
            let raw =
                repo.get_attr_bytes(Path::new(path), name, AttrCheckFlags::FILE_THEN_INDEX)?;
            attrs.push(match AttrValue::from_bytes(raw) {
                AttrValue::Unspecified => Attribute::Unspecified,
                AttrValue::True => Attribute::True,
                AttrValue::False => Attribute::False,
                AttrValue::String(value) => Attribute::Value(value.into()),
                AttrValue::Bytes(_) => bail!("non-UTF-8 Git attribute {name} at {path}"),
            });
        }
        ensure!(
            matches!(attrs[0], Attribute::Unspecified | Attribute::False),
            "external or custom Git filter unsupported at {path}"
        );
        ensure!(
            matches!(attrs[1], Attribute::Unspecified | Attribute::False),
            "working-tree encoding unsupported at {path}"
        );
        ensure!(
            matches!(attrs[2], Attribute::Unspecified | Attribute::False),
            "Git ident expansion unsupported at {path}"
        );
        let config = repo.config()?.snapshot()?;
        let mut settings = Vec::new();
        for name in [
            "core.autocrlf",
            "core.eol",
            "core.safecrlf",
            "core.attributesfile",
        ] {
            settings.push(match config.get_string(name) {
                Ok(v) => Some(v),
                Err(e) if e.code() == ErrorCode::NotFound => None,
                Err(e) => return Err(e.into()),
            });
        }
        lease.verify()?;
        Ok(Self {
            attrs,
            config: settings,
        })
    }
    pub(crate) fn convert(
        &self,
        source: &Source,
        lease: &SyncLease,
        path: &str,
        bytes: &[u8],
        clean: bool,
    ) -> Result<Vec<u8>> {
        ensure!(bytes.len() <= LIMIT, "conversion input exceeds limit");
        ensure!(
            *self == Self::capture(source, lease, path)?,
            "Git attributes/config changed before conversion"
        );
        let result = filter(source.root(), path, bytes, clean)?;
        ensure!(
            *self == Self::capture(source, lease, path)?,
            "Git attributes/config changed during conversion"
        );
        Ok(result)
    }
}

#[repr(C)]
struct GitBuf {
    ptr: *mut c_char,
    reserved: usize,
    size: usize,
}
unsafe extern "C" {
    fn git_repository_open(out: *mut *mut c_void, path: *const c_char) -> c_int;
    fn git_repository_workdir(repo: *mut c_void) -> *const c_char;
    fn git_repository_free(repo: *mut c_void);
    fn git_filter_list_load(
        out: *mut *mut c_void,
        repo: *mut c_void,
        blob: *mut c_void,
        path: *const c_char,
        mode: c_int,
        flags: u32,
    ) -> c_int;
    fn git_filter_list_apply_to_buffer(
        out: *mut GitBuf,
        filters: *mut c_void,
        input: *const c_char,
        len: usize,
    ) -> c_int;
    fn git_filter_list_free(filters: *mut c_void);
    fn git_buf_dispose(buffer: *mut GitBuf);
}
struct Repo(*mut c_void);
impl Drop for Repo {
    fn drop(&mut self) {
        unsafe { git_repository_free(self.0) }
    }
}
struct Filters(*mut c_void);
impl Drop for Filters {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { git_filter_list_free(self.0) }
        }
    }
}
struct Buffer(GitBuf);
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { git_buf_dispose(&mut self.0) }
    }
}
fn filter(root: &Path, path: &str, input: &[u8], clean: bool) -> Result<Vec<u8>> {
    let root_c = CString::new(root.as_os_str().as_bytes())?;
    let path_c = CString::new(path)?;
    let mut repo = ptr::null_mut();
    // git2 initialized libgit2 when Source opened; all handles are freed by RAII.
    ensure!(
        unsafe { git_repository_open(&mut repo, root_c.as_ptr()) } == 0 && !repo.is_null(),
        "open native filter repository"
    );
    let repo = Repo(repo);
    let workdir = unsafe { git_repository_workdir(repo.0) };
    ensure!(!workdir.is_null(), "filter repository has no workdir");
    let workdir = unsafe { CStr::from_ptr(workdir) };
    use std::os::unix::ffi::OsStrExt as _;
    let actual = Path::new(std::ffi::OsStr::from_bytes(workdir.to_bytes())).canonicalize()?;
    ensure!(actual == root, "filter repository root changed");
    let mut list = ptr::null_mut();
    let code = unsafe {
        git_filter_list_load(
            &mut list,
            repo.0,
            ptr::null_mut(),
            path_c.as_ptr(),
            if clean { 1 } else { 0 },
            0,
        )
    };
    let list = Filters(list);
    ensure!(code == 0, "load native Git filters");
    if list.0.is_null() {
        return Ok(input.to_vec());
    }
    let mut out = Buffer(GitBuf {
        ptr: ptr::null_mut(),
        reserved: 0,
        size: 0,
    });
    ensure!(
        unsafe {
            git_filter_list_apply_to_buffer(&mut out.0, list.0, input.as_ptr().cast(), input.len())
        } == 0,
        "apply native Git filters"
    );
    ensure!(out.0.size <= LIMIT, "converted file exceeds limit");
    if out.0.size == 0 {
        return Ok(Vec::new());
    }
    ensure!(!out.0.ptr.is_null(), "native filter returned no buffer");
    Ok(unsafe { std::slice::from_raw_parts(out.0.ptr.cast::<u8>(), out.0.size) }.to_vec())
}
#[cfg(test)]
#[path = "checkout_sync_eol_tests.rs"]
mod tests;
