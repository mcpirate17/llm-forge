//! Explicit operator grants for mutation execution. Read roots also permit
//! execution below them: program aliases control coordinator resolution, not a
//! complete descendant executable allowlist. Bindings detect point-in-time
//! drift, not hostile same-UID mutation or recursive directory content changes.
use crate::{
    candidate_engine_identity::{executable_file, BinaryIdentity},
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    cgroup,
};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_READ_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_READ_FILE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PROGRAM_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOTAL_PROGRAM_BYTES: u64 = 1024 * 1024 * 1024;

/// Describe the actual aggregate cgroup limit without implying RLIMIT_DATA is set.
pub(crate) fn memory_enforcement(
    resources: &cgroup::Limits,
    legacy_cap: Option<crate::mutation_process::MemoryCap>,
    explicitly_requested: bool,
) -> Result<serde_json::Value> {
    ensure!(
        !explicitly_requested || legacy_cap.is_none(),
        "explicit MUTATION_MEMORY_CAP_GIB cannot be enforced by confined mutation execution"
    );
    let bytes = resources
        .memory_mib
        .checked_mul(1024 * 1024)
        .context("execution cgroup memory limit overflows bytes")?;
    Ok(serde_json::json!({
        "mode": "aggregate_cgroup",
        "bytes": bytes,
        "legacy_requested_per_process_data_bytes": legacy_cap.map(|cap| cap.limit),
        "legacy_request_explicit": explicitly_requested,
        "scope": "aggregate memory budget, not per-process RLIMIT_DATA enforcement"
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    source_root: PathBuf,
    scratch_parent: PathBuf,
    cgroup_parent: PathBuf,
    read_only: Vec<PathBuf>,
    programs: BTreeMap<String, Program>,
    resources: cgroup::Limits,
    max_wall_seconds: u64,
    #[serde(default)]
    cargo_workspace: Option<CargoWorkspaceGrant>,
    #[serde(default)]
    python_guest: Option<crate::mutation_fest_guest::GuestConfig>,
    #[serde(default)]
    mull_toolchain: Option<crate::mutation_mull_toolchain::Config>,
}

/// The operator selects the lockfile independently of the seed's own metadata.
/// Confined Cargo discovery must agree before any build or mutation starts.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CargoWorkspaceGrant {
    pub(crate) lockfile: String,
    #[serde(default)]
    pub(crate) seed: Option<crate::mutation_cargo_seed::SeedConfig>,
}
impl CargoWorkspaceGrant {
    fn validate(&self) -> Result<()> {
        conductor_native::mutation_snapshot_inputs::validate_path(&self.lockfile)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            Path::new(&self.lockfile).file_name() == Some(std::ffi::OsStr::new("Cargo.lock")),
            "Cargo seed requires a repository-relative Cargo.lock path"
        );
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    path: PathBuf,
    sha256: String,
}

pub(crate) struct Grants {
    pub(crate) source_root: PathBuf,
    pub(crate) scratch_parent: PathBuf,
    pub(crate) cgroup_parent: PathBuf,
    pub(crate) read_only: Vec<PathBuf>,
    pub(crate) resources: cgroup::Limits,
    pub(crate) max_wall_seconds: u64,
    pub(crate) cargo_workspace: Option<CargoWorkspaceGrant>,
    guest: Option<crate::mutation_fest_guest::HeldGuest>,
    mull: Option<crate::mutation_mull_toolchain::Held>,
    common_git: PathBuf,
    git_dir: PathBuf,
    environment: EnvironmentSelection,
    home: PathBuf,
    protected: Vec<PathBuf>,
    control_path: PathBuf,
    digest: String,
    control: BoundFile,
    directories: Vec<DirectoryBinding>,
    tools: BTreeMap<String, HeldProgram>,
    read_directories: BTreeSet<PathBuf>,
    read_files: Vec<BoundFile>,
}
struct HeldProgram {
    path: PathBuf,
    identity: BinaryIdentity,
    parent: DirectoryBinding,
    held: File,
    stamp: ProgramStamp,
}
type ProgramStamp = (u32, u32, u64, u64, i64, i64, i64, i64);
fn program_stamp(m: &fs::Metadata) -> ProgramStamp {
    (
        m.mode(),
        m.uid(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn normalized(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.as_os_str().as_encoded_bytes().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control)
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "execution grant path must be bounded, absolute and normalized"
    );
    Ok(())
}
fn canonical(path: &Path) -> Result<()> {
    normalized(path)?;
    ensure!(
        path.canonicalize()? == path,
        "execution grant path contains a symlink"
    );
    Ok(())
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn disjoint(a: &Path, b: &Path) -> Result<()> {
    ensure!(
        !overlaps(a, b),
        "execution grant paths overlap: {} and {}",
        a.display(),
        b.display()
    );
    Ok(())
}
fn ancestors(path: &Path) -> Result<()> {
    for parent in path.ancestors().skip(1) {
        let m = fs::symlink_metadata(parent)?;
        // A root-owned sticky temporary parent is safe against other-UID rename;
        // held directory identities are still rechecked. No same-UID immunity.
        ensure!(
            m.is_dir()
                && (m.uid() == 0 || m.uid() == unsafe { libc::geteuid() })
                && (m.mode() & 0o022 == 0 || (m.uid() == 0 && m.mode() & libc::S_ISVTX != 0)),
            "unsafe execution grant ancestor: {}",
            parent.display()
        );
    }
    Ok(())
}
fn directory(path: &Path) -> Result<DirectoryBinding> {
    canonical(path)?;
    ancestors(path)?;
    DirectoryBinding::open(path, None)
}
fn digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "execution program requires lowercase SHA-256"
    );
    Ok(())
}
fn executable(path: &Path) -> Result<File> {
    canonical(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let m = file.metadata()?;
    ensure!(
        m.is_file()
            && m.len() <= MAX_PROGRAM_BYTES
            && m.nlink() == 1
            && m.mode() & 0o111 != 0
            && m.mode() & 0o7022 == 0
            && (m.uid() == 0 || m.uid() == unsafe { libc::geteuid() }),
        "execution program must be safely owned, singly linked and executable"
    );
    Ok(file)
}
impl HeldProgram {
    fn capture(program: Program, remaining: u64) -> Result<Self> {
        digest(&program.sha256)?;
        let parent = directory(program.path.parent().context("program has no parent")?)?;
        let held = executable(&program.path)?;
        ensure!(
            held.metadata()?.len() <= remaining,
            "execution program aggregate byte limit exceeded"
        );
        let stamp = program_stamp(&held.metadata()?);
        let identity = executable_file(held.try_clone()?, || Ok(()))?;
        ensure!(
            program_stamp(&held.metadata()?) == stamp,
            "execution program changed during capture"
        );
        ensure!(
            identity.sha256 == program.sha256,
            "execution program digest mismatch"
        );
        let program = Self {
            path: program.path,
            identity,
            parent,
            held,
            stamp,
        };
        program.verify()?;
        Ok(program)
    }
    fn verify(&self) -> Result<()> {
        ancestors(&self.path)?;
        self.parent.verify()?;
        let file = executable(&self.path)?;
        ensure!(
            program_stamp(&file.metadata()?) == self.stamp,
            "execution program metadata changed"
        );
        ensure!(
            executable_file(file, || Ok(()))? == self.identity,
            "execution program identity changed: {}",
            self.path.display()
        );
        let held = self.held.metadata()?;
        ensure!(
            held.dev() == self.identity.dev
                && held.ino() == self.identity.ino
                && program_stamp(&held) == self.stamp,
            "held execution program identity changed"
        );
        self.parent.verify()
    }
}
// The declaration pins capability paths. File bytes are independently held at
// admission and revalidated, not represented as operator-supplied content pins.
fn read_file(path: &Path, source_root: &Path) -> Result<(BoundFile, usize)> {
    let (observed, bytes) = candidate_inputs::repository_live(path, MAX_READ_FILE_BYTES)?;
    let hash = crate::mutation_campaign::sha256_hex(&bytes);
    let (held, _) =
        candidate_inputs::external_with_tmp(path, &hash, MAX_READ_FILE_BYTES, source_root, None)?;
    observed.verify()?;
    Ok((held, bytes.len()))
}
type EnvironmentSelection = (Option<std::ffi::OsString>, Option<std::ffi::OsString>);
fn environment_selection() -> EnvironmentSelection {
    (std::env::var_os("HOME"), std::env::var_os("CARGO_HOME"))
}
fn protected_roots() -> Result<(PathBuf, Vec<PathBuf>)> {
    let home =
        PathBuf::from(std::env::var_os("HOME").context("HOME required for grant exclusions")?);
    canonical(&home)?;
    let mut roots = vec![PathBuf::from("/sys/fs/cgroup")];
    for name in [".ssh", ".aws", ".gnupg", ".config", ".cargo"] {
        roots.push(home.join(name));
    }
    if let Some(value) = std::env::var_os("CARGO_HOME") {
        let cargo = PathBuf::from(value);
        normalized(&cargo)?;
        // Canonicalize existing values: a symlink must not hide credentials.
        match cargo.canonicalize() {
            Ok(actual) => roots.push(actual),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => roots.push(cargo),
            Err(e) => return Err(e.into()),
        }
    }
    // Credential directories themselves may be symlinks to external storage.
    // Exclude both the declared location and its current canonical target.
    for path in roots.clone() {
        match path.canonicalize() {
            Ok(actual) if actual != path => roots.push(actual),
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok((home, roots))
}
fn validate_document(document: &Document, source_root: &Path) -> Result<()> {
    ensure!(
        document.version == 1,
        "unsupported execution grants version"
    );
    ensure!(
        document.source_root == source_root,
        "execution grants source_root mismatch"
    );
    ensure!(
        (1..=120).contains(&document.read_only.len())
            && (1..=128).contains(&document.programs.len()),
        "execution grants collection limit exceeded"
    );
    ensure!(
        (1..=86400).contains(&document.max_wall_seconds),
        "invalid execution wall ceiling"
    );
    let limits = &document.resources;
    // Same ranges as Cgroup::create; validation must not create a cgroup.
    ensure!(
        (10..=1_024_000).contains(&limits.cpu_millis)
            && (16..=1_048_576).contains(&limits.memory_mib)
            && (1..=4096).contains(&limits.processes),
        "invalid execution cgroup ceilings"
    );
    Ok(())
}

impl Grants {
    pub(crate) fn load(
        path: &Path,
        pin: &str,
        source_root: &Path,
        common_git: &Path,
        git_dir: &Path,
    ) -> Result<Self> {
        canonical(source_root)?;
        canonical(common_git)?;
        canonical(git_dir)?;
        canonical(path)?;
        ancestors(path)?;
        disjoint(path, source_root)?;
        disjoint(path, common_git)?;
        disjoint(path, git_dir)?;
        let (control, bytes) =
            candidate_inputs::external_with_tmp(path, pin, MAX_DOCUMENT, source_root, None)?;
        let text = std::str::from_utf8(&bytes)?;
        let value = crate::tool_schema::parse_document_with_limit(text, MAX_DOCUMENT)?;
        let document: Document = serde_json::from_value(value)?;
        if let Some(workspace) = &document.cargo_workspace {
            workspace.validate()?;
        }
        Self::bind(
            document,
            path,
            pin,
            source_root,
            common_git,
            git_dir,
            control,
        )
    }
    fn bind(
        document: Document,
        path: &Path,
        pin: &str,
        source_root: &Path,
        common_git: &Path,
        git_dir: &Path,
        control: BoundFile,
    ) -> Result<Self> {
        validate_document(&document, source_root)?;
        let limits = document.resources;
        let directories = vec![
            directory(&document.scratch_parent)?,
            directory(&document.cgroup_parent)?,
        ];
        for excluded in [
            source_root,
            common_git,
            git_dir,
            document.cgroup_parent.as_path(),
        ] {
            disjoint(&document.scratch_parent, excluded)?;
        }
        disjoint(path, &document.scratch_parent)?;
        let (home, protected) = protected_roots()?;
        let guest_config = document.python_guest;
        let mull_config = document.mull_toolchain;
        ensure!(
            mull_config.is_none() || (guest_config.is_none() && document.cargo_workspace.is_none()),
            "Mull toolchain grants cannot be combined with Python or Cargo grants"
        );
        let mull_files: BTreeSet<PathBuf> = mull_config
            .as_ref()
            .map(|config| {
                config
                    .files
                    .keys()
                    .cloned()
                    .chain(
                        document
                            .programs
                            .values()
                            .map(|program| program.path.clone()),
                    )
                    .collect()
            })
            .unwrap_or_default();
        let mut grants = Self {
            git_dir: git_dir.into(),
            environment: environment_selection(),
            home,
            protected,
            source_root: source_root.into(),
            common_git: common_git.into(),
            scratch_parent: document.scratch_parent,
            cgroup_parent: document.cgroup_parent,
            read_only: document.read_only,
            resources: limits,
            max_wall_seconds: document.max_wall_seconds,
            cargo_workspace: document.cargo_workspace,
            guest: None,
            mull: None,
            control_path: path.into(),
            digest: pin.into(),
            control,
            directories,
            tools: BTreeMap::new(),
            read_directories: BTreeSet::new(),
            read_files: Vec::new(),
        };
        grants.bind_reads(&mull_files)?;
        grants.bind_programs(document.programs)?;
        if let Some(config) = &mull_config {
            grants.mull = Some(crate::mutation_mull_toolchain::Held::capture(
                config, &grants,
            )?);
        }
        if let Some(config) = &guest_config {
            ensure!(
                grants.cargo_workspace.is_none(),
                "Fest guest and Cargo workspace grants cannot be combined"
            );
            grants.guest = Some(crate::mutation_fest_guest::HeldGuest::capture(
                config, &grants,
            )?);
        }
        grants.verify()?;
        Ok(grants)
    }
    fn bind_programs(&mut self, programs: BTreeMap<String, Program>) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut total_bytes = 0u64;
        for (alias, program) in programs {
            ensure!(
                !alias.is_empty()
                    && alias.len() <= 128
                    && alias
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    && alias != "."
                    && alias != "..",
                "invalid execution program alias"
            );
            ensure!(
                self.allows_program(&program.path),
                "execution program lacks explicit read grant"
            );
            let held = HeldProgram::capture(program, MAX_TOTAL_PROGRAM_BYTES - total_bytes)?;
            total_bytes += held.identity.bytes;
            ensure!(
                seen.insert((held.identity.dev, held.identity.ino)),
                "execution program aliases share an inode"
            );
            self.tools.insert(alias, held);
        }
        Ok(())
    }
    fn bind_reads(&mut self, mull_files: &BTreeSet<PathBuf>) -> Result<()> {
        let mut total = 0usize;
        for (index, read) in self.read_only.iter().enumerate() {
            ensure!(
                !self.home.starts_with(read),
                "read grant exposes HOME or an ancestor"
            );
            for excluded in self.protected.iter().map(PathBuf::as_path).chain([
                self.source_root.as_path(),
                self.common_git.as_path(),
                self.git_dir.as_path(),
                self.scratch_parent.as_path(),
                self.cgroup_parent.as_path(),
                self.control_path.as_path(),
            ]) {
                disjoint(read, excluded)?;
            }
            for previous in &self.read_only[..index] {
                disjoint(read, previous)?;
            }
            canonical(read)?;
            ancestors(read)?;
            if fs::symlink_metadata(read)?.is_dir() {
                self.directories.push(directory(read)?);
                self.read_directories.insert(read.clone());
            } else if !mull_files.contains(read) {
                let (held, length) = read_file(read, &self.source_root)?;
                total = total
                    .checked_add(length)
                    .context("read grant byte count overflow")?;
                ensure!(
                    total <= MAX_TOTAL_READ_FILE_BYTES,
                    "read file grant aggregate byte limit exceeded"
                );
                self.read_files.push(held);
            }
        }
        Ok(())
    }
    fn allows_program(&self, path: &Path) -> bool {
        self.read_only.iter().any(|root| {
            root == path || self.read_directories.contains(root) && path.starts_with(root)
        })
    }
    pub(crate) fn fest_guest(&self) -> Result<&crate::mutation_fest_guest::HeldGuest> {
        self.guest
            .as_ref()
            .context("confined Fest requires a pinned private Python guest")
    }
    pub(crate) fn has_fest_guest(&self) -> bool {
        self.guest.is_some()
    }
    pub(crate) fn mull_toolchain(&self) -> Result<&crate::mutation_mull_toolchain::Held> {
        self.mull
            .as_ref()
            .context("confined Mull requires a pinned toolchain")
    }
    pub(crate) fn has_mull_toolchain(&self) -> bool {
        self.mull.is_some()
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn program(&self, alias: &str) -> Result<&Path> {
        self.tools
            .get(alias)
            .map(|tool| tool.path.as_path())
            .with_context(|| format!("execution program alias is not granted: {alias}"))
    }
    pub(crate) fn program_sha256(&self, alias: &str) -> Result<&str> {
        self.tools
            .get(alias)
            .map(|tool| tool.identity.sha256.as_str())
            .with_context(|| format!("execution program alias is not granted: {alias}"))
    }
    pub(crate) fn programs(&self) -> BTreeMap<String, PathBuf> {
        self.tools
            .iter()
            .map(|(alias, tool)| (alias.clone(), tool.path.clone()))
            .collect()
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            environment_selection() == self.environment,
            "execution grant HOME/CARGO_HOME selection changed"
        );
        let (home, protected) = protected_roots()?;
        ensure!(
            home == self.home && protected == self.protected,
            "execution grant credential-root selection changed"
        );
        ancestors(&self.control_path)?;
        self.control.verify()?;
        if let Some(guest) = &self.guest {
            guest.verify()?;
        }
        if let Some(mull) = &self.mull {
            mull.verify()?;
        }
        for path in self
            .read_only
            .iter()
            .chain([&self.scratch_parent, &self.cgroup_parent])
        {
            ancestors(path)?;
        }
        for directory in &self.directories {
            directory.verify()?;
        }
        for file in &self.read_files {
            file.verify()?;
        }
        for program in self.tools.values() {
            program.verify()?;
        }
        Ok(())
    }
    pub(crate) fn validate_private_root(&self, root: &Path) -> Result<()> {
        canonical(root)?;
        ensure!(
            root != self.scratch_parent && root.starts_with(&self.scratch_parent),
            "private execution root must be below granted scratch_parent"
        );
        let m = fs::symlink_metadata(root)?;
        ensure!(
            m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
            "private execution root must be operator-owned mode0700"
        );
        for excluded in self.read_only.iter().map(PathBuf::as_path).chain([
            self.source_root.as_path(),
            self.common_git.as_path(),
            self.git_dir.as_path(),
            self.control_path.as_path(),
            self.cgroup_parent.as_path(),
        ]) {
            disjoint(root, excluded)?;
        }
        self.verify()
    }
}

#[cfg(test)]
#[path = "mutation_execution_grants_tests.rs"]
mod tests;

/// Narrow shared admission primitives for an outbound endpoint; the caller
/// additionally restricts every read root to the harness operator's grants.
pub(crate) struct OutboundBindings {
    program: HeldProgram,
    directories: Vec<DirectoryBinding>,
    files: Vec<BoundFile>,
    environment: EnvironmentSelection,
    home: PathBuf,
    protected: Vec<PathBuf>,
    source: PathBuf,
    git: Option<(PathBuf, PathBuf, PathBuf)>,
    report_only: bool,
}
fn outbound_covers(reads: &[PathBuf], path: &Path) -> bool {
    reads
        .iter()
        .any(|root| path == root || root.is_dir() && path.starts_with(root))
}
fn bind_outbound_pins(
    reads: &[PathBuf],
    pins: &[(PathBuf, String)],
    state: &Path,
    files: &mut Vec<BoundFile>,
    total: &mut usize,
) -> Result<()> {
    for (path, hash) in pins {
        ensure!(
            outbound_covers(reads, path),
            "outbound pinned input lacks read grant"
        );
        canonical(path)?;
        ancestors(path)?;
        let (held, bytes) =
            candidate_inputs::external_with_tmp(path, hash, MAX_READ_FILE_BYTES, state, None)?;
        *total = total
            .checked_add(bytes.len())
            .context("outbound pin byte overflow")?;
        ensure!(
            *total <= MAX_TOTAL_READ_FILE_BYTES,
            "outbound pinned file limit exceeded"
        );
        files.push(held);
    }
    Ok(())
}
struct OutboundCapture<'a> {
    program: &'a Path,
    hash: &'a str,
    reads: &'a [PathBuf],
    pins: &'a [(PathBuf, String)],
    source: &'a Path,
    state: &'a Path,
    allowed: &'a [PathBuf],
    report_only: bool,
}
struct ReportGitInputs {
    git: Option<(PathBuf, PathBuf)>,
    git_dir: Option<PathBuf>,
    git_identity: Option<(PathBuf, PathBuf, PathBuf)>,
    directories: Vec<DirectoryBinding>,
    report_git: PathBuf,
}
fn outbound_git_inputs(source: &Path, report_only: bool) -> Result<ReportGitInputs> {
    let git = if report_only {
        None
    } else {
        crate::repository::checkout_of_checked(source)?
    };
    let git_dir = match &git {
        Some((root, _)) => Some(git2::Repository::open(root)?.path().canonicalize()?),
        None => None,
    };
    let git_identity = git.as_ref().map(|(root, common)| {
        (
            root.clone(),
            common.clone(),
            git_dir.clone().expect("checkout has git directory"),
        )
    });
    let report_git = source.join(".git");
    let mut directories = if report_only {
        canonical(source)?;
        ancestors(source)?;
        vec![
            DirectoryBinding::repository(source, None)?,
            DirectoryBinding::repository(&report_git, None)?,
        ]
    } else {
        vec![directory(source)?]
    };
    if let Some((_, common, git_dir)) = &git_identity {
        directories.push(directory(common)?);
        if git_dir != common {
            directories.push(directory(git_dir)?);
        }
    }
    Ok(ReportGitInputs {
        git,
        git_dir,
        git_identity,
        directories,
        report_git,
    })
}
impl OutboundBindings {
    pub(crate) fn capture(
        program: &Path,
        hash: &str,
        reads: &[PathBuf],
        pins: &[(PathBuf, String)],
        source: &Path,
        state: &Path,
        allowed: &[PathBuf],
    ) -> Result<Self> {
        Self::capture_inner(OutboundCapture {
            program,
            hash,
            reads,
            pins,
            source,
            state,
            allowed,
            report_only: false,
        })
    }
    pub(crate) fn capture_report(
        program: &Path,
        hash: &str,
        reads: &[PathBuf],
        source: &Path,
        state: &Path,
        allowed: &[PathBuf],
    ) -> Result<Self> {
        Self::capture_inner(OutboundCapture {
            program,
            hash,
            reads,
            pins: &[],
            source,
            state,
            allowed,
            report_only: true,
        })
    }
    fn capture_inner(input: OutboundCapture<'_>) -> Result<Self> {
        let OutboundCapture {
            program,
            hash,
            reads,
            pins,
            source,
            state,
            allowed,
            report_only,
        } = input;
        ensure!(
            (1..=120).contains(&reads.len()),
            "invalid outbound read grant count"
        );
        ensure!(pins.len() <= 128, "too many outbound pins");
        let (home, protected) = protected_roots()?;
        let ReportGitInputs {
            git,
            git_dir,
            git_identity,
            mut directories,
            report_git,
        } = outbound_git_inputs(source, report_only)?;
        let mut files = Vec::new();
        let mut total = 0usize;
        for (index, read) in reads.iter().enumerate() {
            canonical(read)?;
            ancestors(read)?;
            ensure!(
                read.starts_with(source) || allowed.iter().any(|a| read.starts_with(a)),
                "outbound read exceeds operator grants"
            );
            ensure!(!home.starts_with(read), "outbound read exposes HOME");
            disjoint(read, state)?;
            for path in &protected {
                disjoint(read, path)?;
            }
            if report_only {
                disjoint(read, &report_git)?;
            }
            if let Some((root, common)) = &git {
                disjoint(read, &root.join(".git"))?;
                disjoint(read, common)?;
            }
            if let Some(path) = &git_dir {
                disjoint(read, path)?;
            }
            for previous in &reads[..index] {
                disjoint(read, previous)?;
            }
            // The exact executable is pinned and reverified by HeldProgram below.
            // Do not apply the smaller data-file bound to the same file twice.
            if read == program {
                continue;
            }
            if fs::symlink_metadata(read)?.is_dir() {
                directories.push(directory(read)?);
            } else {
                let (file, size) = read_file(read, state)?;
                total = total
                    .checked_add(size)
                    .context("outbound read byte overflow")?;
                ensure!(
                    total <= MAX_TOTAL_READ_FILE_BYTES,
                    "outbound read file limit exceeded"
                );
                files.push(file);
            }
        }
        ensure!(
            outbound_covers(reads, program),
            "outbound executable lacks read grant"
        );
        let program = HeldProgram::capture(
            Program {
                path: program.into(),
                sha256: hash.into(),
            },
            MAX_PROGRAM_BYTES,
        )?;
        bind_outbound_pins(reads, pins, state, &mut files, &mut total)?;
        let result = Self {
            program,
            directories,
            files,
            environment: environment_selection(),
            home,
            protected,
            source: source.into(),
            git: git_identity,
            report_only,
        };
        result.verify()?;
        Ok(result)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            environment_selection() == self.environment
                && protected_roots()? == (self.home.clone(), self.protected.clone()),
            "outbound credential roots changed"
        );
        if self.report_only {
            ancestors(&self.source)?;
        } else {
            let current = crate::repository::checkout_of_checked(&self.source)?;
            let current = current
                .map(|(root, common)| -> Result<_> {
                    let git_dir = git2::Repository::open(&root)?.path().canonicalize()?;
                    Ok((root, common, git_dir))
                })
                .transpose()?;
            ensure!(current == self.git, "outbound source Git metadata changed");
        }
        self.program.verify()?;
        for directory in &self.directories {
            directory.verify()?;
        }
        for file in &self.files {
            file.verify()?;
        }
        Ok(())
    }
}
