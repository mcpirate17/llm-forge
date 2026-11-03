//! Pinned, no-follow directories and metadata-only interpreter discovery.
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

pub(super) const MAX_ENTRIES: usize = 100_000;
pub(super) const MAX_FILE: usize = 64 * 1024 * 1024;
pub(super) const MAX_TOTAL: usize = 2 * 1024 * 1024 * 1024;
pub(super) fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
fn open_directory(path: &Path) -> Result<(Dir, PathBuf)> {
    ensure!(!path.as_os_str().is_empty(), "virtualenv path is empty");
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        absolute.as_os_str().len() <= 4096 && absolute.components().count() <= 256,
        "virtualenv path exceeds limits"
    );
    let mut ancestors = vec![Dir::open_ambient_dir("/", cap_std::ambient_authority())?];
    let mut normalized = PathBuf::from("/");
    for part in absolute.components() {
        match part {
            Component::Normal(name) => {
                let file = ancestors
                    .last()
                    .context("directory ancestor missing")?
                    .open_with(name, &directory_options())
                    .context("virtualenv ancestors must be real directories without symlinks")?;
                ancestors.push(Dir::from_std_file(file.into_std()));
                normalized.push(name);
            }
            Component::ParentDir => {
                ensure!(
                    ancestors.len() > 1,
                    "virtualenv path escapes filesystem root"
                );
                ancestors.pop();
                normalized.pop();
            }
            Component::RootDir | Component::CurDir => (),
            _ => bail!("unsupported virtualenv path"),
        }
    }
    Ok((
        ancestors.pop().context("virtualenv directory missing")?,
        normalized,
    ))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub length: u64,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    pub fn capture(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    pub fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
}
pub(super) struct Bin {
    pub dir: Dir,
    pub path: PathBuf,
    venv: PathBuf,
    root_identity: (u64, u64),
    identity: (u64, u64),
}
impl Bin {
    pub fn open(venv: &Path) -> Result<Self> {
        let (root, venv) = open_directory(venv)?;
        let root_identity = Stamp::capture(&root.dir_metadata()?).identity();
        let bin = root.open_with("bin", &directory_options()).context("both virtualenvs must contain a real bin directory; bin directories must not be symlinks")?;
        let dir = Dir::from_std_file(bin.into_std());
        let identity = Stamp::capture(&dir.dir_metadata()?).identity();
        let path = venv.join("bin");
        ensure!(
            path.as_os_str().len() <= 4096,
            "virtualenv bin path exceeds limit"
        );
        Ok(Self {
            dir,
            path,
            venv,
            root_identity,
            identity,
        })
    }
    pub fn verify(&self) -> Result<()> {
        let current = Self::open(&self.venv)?;
        ensure!(
            self.root_identity == current.root_identity
                && self.identity == current.identity
                && self.identity == Stamp::capture(&self.dir.dir_metadata()?).identity(),
            "virtualenv/bin directory identity changed"
        );
        Ok(())
    }
    pub fn identity(&self) -> (u64, u64) {
        self.identity
    }
    pub fn distinct(&self, other: &Self) -> Result<()> {
        ensure!(
            self.root_identity != other.root_identity && self.identity != other.identity,
            "source and destination virtualenvs and bin directories must differ"
        );
        Ok(())
    }
    pub fn lease(&self) -> Result<crate::file_lock::Exclusive> {
        let file = self.dir.open_with(".", &directory_options())?;
        crate::file_lock::Exclusive::try_new(
            file.into_std(),
            "virtualenv console-script relocation",
        )
    }
    pub fn sync(&self) -> Result<()> {
        self.dir.open_with(".", &directory_options())?.sync_all()?;
        Ok(())
    }
}
pub(super) fn names(bin: &Bin) -> Result<(BTreeSet<OsString>, Stamp)> {
    let before = Stamp::capture(&bin.dir.dir_metadata()?);
    let mut names = BTreeSet::new();
    let mut visited = 0usize;
    for entry in bin.dir.entries()? {
        visited += 1;
        ensure!(visited <= MAX_ENTRIES, "source bin inventory exceeds limit");
        let name = entry?.file_name();
        ensure!(
            name.as_bytes().len() <= 255,
            "launcher filename exceeds limit"
        );
        names.insert(name);
    }
    ensure!(
        before == Stamp::capture(&bin.dir.dir_metadata()?),
        "source bin changed during inventory"
    );
    Ok((names, before))
}

type TargetIdentity = (u64, u64, u32, u64, i64, i64);
fn target_identity(path: &Path) -> Result<Option<TargetIdentity>> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context("inspect source Python executable target metadata")
        }
    };
    Ok(metadata.is_file().then(|| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        )
    }))
}
struct Interpreter {
    name: OsString,
    identity: TargetIdentity,
    symlink: Option<(Stamp, PathBuf)>,
}
pub(super) struct Interpreters {
    pub replacements: BTreeMap<Vec<u8>, OsString>,
    observed: Vec<Interpreter>,
}
impl Interpreters {
    pub fn discover(source: &Bin, names: &BTreeSet<OsString>) -> Result<Self> {
        let pattern = regex::Regex::new(r"^python(?:\d+(?:\.\d+)?)?$")?;
        let mut out = Self {
            replacements: BTreeMap::new(),
            observed: Vec::new(),
        };
        for name in names {
            if !name.to_str().is_some_and(|name| pattern.is_match(name)) {
                continue;
            }
            let before = source.dir.symlink_metadata(name)?;
            if !before.is_file() && !before.file_type().is_symlink() {
                continue;
            }
            let symlink = if before.file_type().is_symlink() {
                Some((
                    Stamp::capture(&before),
                    source.dir.read_link_contents(name)?,
                ))
            } else {
                None
            };
            // Narrow exception: follow interpreter names for metadata only, with
            // the OS symlink-depth bound. Never open/read/execute their referents.
            let Some(identity) = target_identity(&source.path.join(name))? else {
                continue;
            };
            let mut shebang = b"#!".to_vec();
            shebang.extend_from_slice(source.path.join(name).as_os_str().as_bytes());
            out.replacements.insert(shebang, name.clone());
            out.observed.push(Interpreter {
                name: name.clone(),
                identity,
                symlink,
            });
        }
        ensure!(
            !out.observed.is_empty(),
            "no Python executable under {}",
            source.path.display()
        );
        out.verify(source)?;
        Ok(out)
    }
    pub fn verify(&self, source: &Bin) -> Result<()> {
        source.verify()?;
        for item in &self.observed {
            let named = source.dir.symlink_metadata(&item.name)?;
            if let Some((stamp, target)) = &item.symlink {
                ensure!(
                    *stamp == Stamp::capture(&named)
                        && *target == source.dir.read_link_contents(&item.name)?,
                    "source Python symlink changed"
                );
            } else {
                ensure!(named.is_file(), "source Python executable changed type");
            }
            ensure!(
                target_identity(&source.path.join(&item.name))? == Some(item.identity),
                "source Python executable target changed"
            );
        }
        source.verify()?;
        Ok(())
    }
}
