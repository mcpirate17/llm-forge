use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Fixture {
    pub root: PathBuf,
    pub source: PathBuf,
    pub destination: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "forge-relocation-{}-{now}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let source = root.join("source-venv");
        let destination = root.join("destination-venv");
        for venv in [&source, &destination] {
            fs::create_dir(venv).unwrap();
            fs::create_dir(venv.join("bin")).unwrap();
            fs::write(
                venv.join("bin/python3"),
                b"not an executable; fixture metadata only\n",
            )
            .unwrap();
        }
        Self {
            root,
            source,
            destination,
        }
    }
    pub fn payload(&self, interpreter: &str, rest: &[u8]) -> Vec<u8> {
        let mut bytes = format!("#!{}/bin/{interpreter}\n", self.source.display()).into_bytes();
        bytes.extend_from_slice(rest);
        bytes
    }
    pub fn linked(&self, name: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
        let source = self.source.join("bin").join(name);
        let destination = self.destination.join("bin").join(name);
        fs::write(&source, bytes).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).unwrap();
        fs::hard_link(&source, &destination).unwrap();
        (source, destination)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    inode: (u64, u64),
    mode: u32,
    bytes: Vec<u8>,
}
pub fn image(root: &Path) -> BTreeMap<PathBuf, Image> {
    fn walk(root: &Path, relative: &Path, out: &mut BTreeMap<PathBuf, Image>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let path = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let bytes = if metadata.is_dir() {
                Vec::new()
            } else if metadata.file_type().is_symlink() {
                fs::read_link(entry.path())
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else if metadata.is_file() {
                fs::read(entry.path()).unwrap()
            } else {
                Vec::new()
            };
            out.insert(
                path.clone(),
                Image {
                    inode: (metadata.dev(), metadata.ino()),
                    mode: metadata.mode(),
                    bytes,
                },
            );
            if metadata.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, Path::new(""), &mut out);
    out
}
