#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
const CRATE: &str = "version_check";
const VERSION: &str = "0.9.5";
const CHECKSUM: &str = "0b928f33d975fc6ad9f86c8f283853ad26bdd5b10b7f1542aa2fa15e2289105a";
const INDEX: &str = ".cache/ve/rs/version_check";
struct Saved {
    path: PathBuf,
    bytes: Vec<u8>,
    metadata: fs::Metadata,
}
fn stable(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
        && a.nlink() == b.nlink()
        && a.mode() == b.mode()
}
fn capture(path: &Path, maximum: usize) -> Saved {
    let before = fs::symlink_metadata(path).expect("required cached fixture file missing");
    assert!(
        before.is_file() && before.len() <= maximum as u64,
        "unsafe/oversize fixture {}",
        path.display()
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .unwrap();
    assert!(stable(&before, &file.metadata().unwrap()));
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= maximum && bytes.len() as u64 == before.len());
    assert!(stable(&before, &file.metadata().unwrap()));
    assert!(stable(&before, &fs::symlink_metadata(path).unwrap()));
    Saved {
        path: path.into(),
        bytes,
        metadata: before,
    }
}
fn unchanged(saved: &[Saved]) {
    for file in saved {
        let current = capture(&file.path, file.bytes.len());
        assert_eq!(
            current.bytes,
            file.bytes,
            "changed source/cache file {}",
            file.path.display()
        );
        assert!(
            stable(&current.metadata, &file.metadata),
            "changed identity/metadata {}",
            file.path.display()
        );
    }
}
fn cached_files(home: &Path) -> (String, Vec<Saved>) {
    let mut registries = Vec::new();
    for (index, entry) in fs::read_dir(home.join("registry/cache"))
        .expect("cache seed lacks registry/cache")
        .enumerate()
    {
        assert!(index < 64, "fixture registry search exceeds64entries");
        let entry = entry.unwrap();
        let name = entry
            .file_name()
            .into_string()
            .expect("registry name must be UTF8");
        if name.starts_with("index.crates.io-")
            && entry
                .path()
                .join(format!("{CRATE}-{VERSION}.crate"))
                .is_file()
        {
            registries.push(name);
        }
    }
    registries.sort();
    let registry = registries
        .first()
        .expect("seed must cache version_check0.9.5 sparse crates.io fixture")
        .clone();
    let paths = [
        home.join("registry/cache")
            .join(&registry)
            .join(format!("{CRATE}-{VERSION}.crate")),
        home.join("registry/index").join(&registry).join(INDEX),
        home.join("registry/index")
            .join(&registry)
            .join("config.json"),
    ];
    let files = vec![
        capture(&paths[0], 128 * 1024),
        capture(&paths[1], 1024 * 1024),
        capture(&paths[2], 64 * 1024),
    ];
    assert_eq!(format!("{:x}", Sha256::digest(&files[0].bytes)), CHECKSUM);
    let record = files[1]
        .bytes
        .split(|b| *b == 0)
        .filter(|p| p.first() == Some(&b'{'))
        .map(|p| serde_json::from_slice::<Value>(p).expect("malformed cached sparse-index JSON"))
        .find(|v| v["vers"] == VERSION)
        .expect("sparse entry lacks exact pinned version");
    assert_eq!(record["name"], CRATE);
    assert_eq!(record["cksum"], CHECKSUM);
    assert_eq!(record["deps"], serde_json::json!([]));
    assert_eq!(record["yanked"], false);
    let config: Value = serde_json::from_slice(&files[2].bytes).unwrap();
    assert!(config["dl"]
        .as_str()
        .unwrap()
        .starts_with("https://static.crates.io/crates"));
    (registry, files)
}
fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn minimal_seed(home: &Path, registry: &str, files: &[Saved]) -> Vec<Saved> {
    let paths = [
        home.join("registry/cache")
            .join(registry)
            .join(format!("{CRATE}-{VERSION}.crate")),
        home.join("registry/index").join(registry).join(INDEX),
        home.join("registry/index")
            .join(registry)
            .join("config.json"),
    ];
    for (path, file) in paths.iter().zip(files) {
        write(path, &file.bytes);
        assert_ne!(fs::metadata(path).unwrap().ino(), file.metadata.ino());
    }
    write(
        &home.join("config.toml"),
        b"INVALID SEED CONFIG MUST NOT BE LOADED [\n",
    );
    paths
        .into_iter()
        .map(|p| capture(&p, 1024 * 1024))
        .chain(std::iter::once(capture(&home.join("config.toml"), 1024)))
        .collect()
}
fn candidate(root: &Path) -> Vec<Saved> {
    write(
        &root.join("forge.toml"),
        b"schema_version=1\n[conductor]\ncrate_roster='roster.toml'\n",
    );
    write(
        &root.join("roster.toml"),
        b"[crates]\ntested=['pkg']\nlinted=['pkg']\n[globs]\nmanifests=['pkg/Cargo.toml']\n",
    );
    write(&root.join("pkg/Cargo.toml"),b"[package]\nname='seed_probe'\nversion='0.1.0'\nedition='2021'\n[dependencies]\nversion_check='=0.9.5'\n");
    write(&root.join("pkg/src/lib.rs"),b"pub fn accepts_version() -> bool { version_check::Version::parse(\"1.98.0\").is_some() }\n");
    write(&root.join("pkg/Cargo.lock"),format!("version = 4\n\n[[package]]\nname = \"seed_probe\"\nversion = \"0.1.0\"\ndependencies = [\"version_check\"]\n\n[[package]]\nname = \"version_check\"\nversion = \"0.9.5\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{CHECKSUM}\"\n").as_bytes());
    [
        "forge.toml",
        "roster.toml",
        "pkg/Cargo.toml",
        "pkg/Cargo.lock",
        "pkg/src/lib.rs",
    ]
    .iter()
    .map(|p| capture(&root.join(p), 64 * 1024))
    .collect()
}
fn invoke(host: &support::Host, bin: &Path, seed: Option<&Path>) -> Output {
    let mut command = Command::new(support::forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", host.0.join("private-home"))
        .env("TMPDIR", host.0.join("tmp"))
        .env("FORGE_LEDGER_DISABLE", "1")
        .env("FORGE_CARGO_BIN", bin)
        .env("FORGE_CARGO_EXTRA_PATH", "/usr/bin")
        .current_dir(host.0.join("repo"))
        .args([
            "candidate",
            "cargo-lint",
            "--mode",
            "clippy",
            "--execute",
            "--",
            "pkg/src/lib.rs",
        ]);
    if let Some(seed) = seed {
        command.env("FORGE_CARGO_HOME_SEED", seed);
    }
    forge::process_probe::run(&mut command, Duration::from_secs(45), 1024 * 1024).unwrap()
}
fn clean(host: &support::Host) {
    assert_eq!(fs::read_dir(host.0.join("tmp")).unwrap().count(), 0);
    assert!(!host.0.join("repo/target").exists());
    assert!(!host.0.join("repo/pkg/target").exists());
    assert!(!host.0.join("repo/.cargo-home").exists());
}
#[test]
#[ignore = "requires FORGE_TEST_CARGO_BIN and FORGE_TEST_CARGO_HOME_SEED with cached version_check0.9.5"]
fn real_offline_clippy_uses_minimal_private_registry_seed_without_loading_its_config() {
    let bin = PathBuf::from(
        std::env::var_os("FORGE_TEST_CARGO_BIN")
            .expect("set explicit native Rust toolchain bin directory"),
    )
    .canonicalize()
    .unwrap();
    let source = PathBuf::from(
        std::env::var_os("FORGE_TEST_CARGO_HOME_SEED")
            .expect("set explicit read-only Cargo cache fixture home"),
    )
    .canonicalize()
    .unwrap();
    assert!(bin.is_dir() && source.is_dir());
    for tool in ["cargo", "rustc", "rustdoc", "cargo-clippy", "clippy-driver"] {
        assert!(bin.join(tool).is_file(), "missing explicit tool {tool}");
    }
    let host = support::Host::new();
    fs::create_dir(host.0.join("tmp")).unwrap();
    let (registry, original) = cached_files(&source);
    let owned_seed = host.0.join("seed");
    let seed_files = minimal_seed(&owned_seed, &registry, &original);
    let candidate_files = candidate(&host.0.join("repo"));
    let without = invoke(&host, &bin, None);
    assert!(
        !without.status.success(),
        "empty private cache must fail offline resolution"
    );
    let diagnostic = String::from_utf8_lossy(&without.stderr);
    assert!(
        diagnostic.contains("version_check") && diagnostic.contains("offline"),
        "{diagnostic}"
    );
    clean(&host);
    unchanged(&candidate_files);
    unchanged(&original);
    unchanged(&seed_files);
    let with = invoke(&host, &bin, Some(&owned_seed));
    assert!(
        with.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&with.stdout),
        String::from_utf8_lossy(&with.stderr)
    );
    clean(&host);
    unchanged(&candidate_files);
    unchanged(&original);
    unchanged(&seed_files);
}
