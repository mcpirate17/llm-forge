//! Rust-owned fixture construction for the installed-wheel resource contracts.

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
static BASELINE: OnceLock<BaselineDir> = OnceLock::new();
const CHILD: &str = env!("CARGO_BIN_EXE_package_resources_child");

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hook {
    None,
    AppendPath {
        path: PathBuf,
    },
    ShadowPath {
        path: PathBuf,
    },
    ReplaceAfterOpen {
        resource: PathBuf,
        replacement: PathBuf,
    },
    FifoBeforeOpen {
        resource: PathBuf,
    },
    SymlinkBeforeOpen {
        resource: PathBuf,
        target: PathBuf,
    },
    IntermediateBeforeOpen {
        nested: PathBuf,
        external: PathBuf,
    },
}

pub struct TempCase(PathBuf);

impl TempCase {
    pub fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "forge-package-resources-{}-{}-{label}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create package resource fixture root");
        Self(root)
    }
    pub fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove package resource fixture root");
    }
}

struct BaselineDir {
    root: PathBuf,
}

pub struct BaselineLease(&'static BaselineDir);

impl BaselineLease {
    pub fn acquire() -> Self {
        Self(baseline_root())
    }
}

extern "C" fn cleanup_baseline() {
    if let Some(baseline) = BASELINE.get() {
        if let Err(error) = fs::remove_dir_all(&baseline.root) {
            eprintln!("cannot remove package resource baseline: {error}");
            unsafe { libc::_exit(1) };
        }
    }
}

fn baseline_root() -> &'static BaselineDir {
    BASELINE.get_or_init(|| {
        let root = create_baseline();
        assert_eq!(unsafe { libc::atexit(cleanup_baseline) }, 0);
        BaselineDir { root }
    })
}

fn python() -> PathBuf {
    std::env::var_os("PYO3_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python3"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve Forge repository root")
}

fn digest(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(data))
}

pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn write_wheel(root: &Path, wheel: &Path) {
    let source = repo_root().join("src/conductor");
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    files.insert("conductor/__init__.py".into(), Vec::new());
    for name in ["project_context.py", "package_resources.py"] {
        files.insert(
            format!("conductor/{name}"),
            fs::read(source.join(name)).expect("read current package source"),
        );
    }
    for (name, body) in [
        ("conductor/fixture.txt", b"conductor-installed-resource\n".as_slice()),
        ("tooling/fixture.txt", b"tooling-installed-resource\n".as_slice()),
        ("tooling/nested/fixture.txt", b"nested-installed-resource\n".as_slice()),
        (
            "conductor_tooling-9.9.9.dist-info/METADATA",
            b"Metadata-Version: 2.1\nName: conductor-tooling\nVersion: 9.9.9\n".as_slice(),
        ),
        (
            "conductor_tooling-9.9.9.dist-info/WHEEL",
            b"Wheel-Version: 1.0\nGenerator: package-resources-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n".as_slice(),
        ),
    ] {
        files.insert(name.into(), body.to_vec());
    }
    let record_name = "conductor_tooling-9.9.9.dist-info/RECORD";
    let mut rows = files
        .iter()
        .map(|(name, body)| format!("{name},sha256={},{}", digest(body), body.len()))
        .collect::<Vec<_>>();
    rows.push(format!("{record_name},,"));
    files.insert(
        record_name.into(),
        format!("{}\n", rows.join("\n")).into_bytes(),
    );

    for (name, body) in &files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    write_stored_zip(wheel, &files);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320_u32 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn write_stored_zip(wheel: &Path, files: &BTreeMap<String, Vec<u8>>) {
    let mut archive = Vec::new();
    let mut directory = Vec::new();
    for (name, body) in files {
        let name_bytes = name.as_bytes();
        let offset = u32::try_from(archive.len()).expect("small wheel offset");
        let size = u32::try_from(body.len()).expect("small wheel entry");
        let name_len = u16::try_from(name_bytes.len()).expect("small wheel path");
        let crc = crc32(body);
        archive.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&20_u16.to_le_bytes());
        archive.extend_from_slice(&[0; 4]); // flags and stored compression method
        archive.extend_from_slice(&[0; 4]); // DOS timestamp
        archive.extend_from_slice(&crc.to_le_bytes());
        archive.extend_from_slice(&size.to_le_bytes());
        archive.extend_from_slice(&size.to_le_bytes());
        archive.extend_from_slice(&name_len.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(name_bytes);
        archive.extend_from_slice(body);

        directory.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        directory.extend_from_slice(&20_u16.to_le_bytes()); // version made by
        directory.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        directory.extend_from_slice(&[0; 8]); // flags, method, DOS timestamp
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_len.to_le_bytes());
        directory.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attrs
        directory.extend_from_slice(&(0o100644_u32 << 16).to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name_bytes);
    }
    let directory_offset = u32::try_from(archive.len()).expect("small wheel offset");
    let directory_size = u32::try_from(directory.len()).expect("small wheel directory");
    archive.extend_from_slice(&directory);
    archive.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    archive.extend_from_slice(&[0; 4]); // disk numbers
    let count = u16::try_from(files.len()).expect("small wheel entry count");
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&directory_size.to_le_bytes());
    archive.extend_from_slice(&directory_offset.to_le_bytes());
    archive.extend_from_slice(&0_u16.to_le_bytes());
    fs::write(wheel, archive).expect("write Rust-owned wheel");
}

fn create_baseline() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "forge-package-resource-baseline-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).expect("create wheel baseline");
    let wheel_root = root.join("wheel-root");
    let wheel = root.join("conductor_tooling-9.9.9-py3-none-any.whl");
    fs::create_dir_all(&wheel_root).unwrap();
    write_wheel(&wheel_root, &wheel);
    let environment = root.join("installed-baseline");
    let output = Command::new(python())
        .args(["-I", "-B", "-m", "venv", "--clear"])
        .arg(&environment)
        .output()
        .expect("create isolated baseline venv");
    assert_success(&output, "create baseline venv");
    let interpreter = environment.join("bin/python");
    let output = Command::new("timeout")
        .arg("30")
        .arg(&interpreter)
        .args(["-I", "-B", "-m", "pip", "install", "--no-deps"])
        .arg(&wheel)
        .output()
        .expect("install local test wheel");
    assert_ne!(
        output.status.code(),
        Some(124),
        "wheel install exceeded 30s"
    );
    assert_success(&output, "install local test wheel");
    root
}

pub fn installed_tooling(case: &TempCase, lease: &BaselineLease) -> (PathBuf, PathBuf) {
    let baseline = lease.0.root.join("installed-baseline");
    let environment = case.root().join("private-environment");
    copy_tree(&baseline, &environment);
    let python_dir = fs::read_dir(environment.join("lib"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("python")
        })
        .expect("find baseline Python directory");
    let site = python_dir.join("site-packages");
    assert!(site.is_dir());
    (environment.join("bin/python"), site)
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            copy_tree(&from, &to);
        } else if ty.is_symlink() {
            symlink(fs::read_link(from).unwrap(), to).unwrap();
        } else if ty.is_file() {
            fs::copy(from, to).unwrap();
        }
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn run_reader(interpreter: &Path, site: &Path, package: &str, name: &str, hook: Hook) -> Value {
    let request = serde_json::json!({
        "package":package, "name":name, "site":site, "hook":hook,
    });
    let mut child = Command::new("timeout")
        .arg("15")
        .arg(CHILD)
        .arg(interpreter)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run installed package resource reader");
    child
        .stdin
        .take()
        .expect("reader stdin")
        .write_all(request.to_string().as_bytes())
        .expect("send Rust reader request");
    let output = child.wait_with_output().expect("finish resource reader");
    assert_ne!(
        output.status.code(),
        Some(124),
        "resource reader exceeded 15s timeout"
    );
    assert_success(&output, "package resource reader");
    serde_json::from_slice(&output.stdout).expect("decode reader result")
}

pub fn digest_files(root: &Path) -> BTreeMap<String, String> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, out);
            } else if fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, sha256_hex(&fs::read(path).unwrap()));
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

pub fn rust_test_temp(label: &str) -> TempCase {
    TempCase::new(label)
}
