use super::*;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt},
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    package: PathBuf,
    output: PathBuf,
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())
        .unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        // Capture forbids TMPDIR. Keep fixtures outside it without mutating the
        // process environment, including when the test harness sets TMPDIR.
        let scratch = std::env::var_os("TMPDIR").map(PathBuf::from);
        let base = ["/tmp", "/var/tmp"]
            .into_iter()
            .find(|base| {
                scratch
                    .as_ref()
                    .is_none_or(|scratch| !Path::new(base).starts_with(scratch))
            })
            .expect("test needs a temporary root outside TMPDIR");
        let temp = tempfile::Builder::new()
            .prefix("forge-runner-bundle-")
            .tempdir_in(base)
            .unwrap();
        let repo = temp.path().join("source");
        let package = temp.path().join("installed");
        let output_dir = temp.path().join("private");
        for path in [&repo, &package, &output_dir] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        git2::Repository::init(&repo).unwrap();
        for key in COMPONENTS.iter().chain(BINDINGS.iter()) {
            write(
                &package.join(key),
                format!("installed bytes for {key}\n").as_bytes(),
                0o600,
            );
        }
        Self {
            _temp: temp,
            repo,
            package,
            output: output_dir.join("bundle.json"),
        }
    }

    fn request(&self) -> Request<'_> {
        Request {
            repo: &self.repo,
            package_root: &self.package,
            output: &self.output,
            lineage: None,
            external_invariant_probe: None,
            exclude_roots: &[],
        }
    }

    fn no_output(&self) {
        assert!(!self.output.exists());
        assert_eq!(
            fs::read_dir(self.output.parent().unwrap()).unwrap().count(),
            0
        );
    }
}

#[test]
fn runner_bundle_exact_schema_hashes_installed_bytes_and_private_output() {
    let fixture = Fixture::new();
    let generated = generate(fixture.request()).unwrap();
    let bytes = fs::read(&fixture.output).unwrap();
    assert_eq!(
        generated.manifest_sha256,
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(generated.manifest_bytes, bytes.len());
    let manifest: Manifest = serde_json::from_slice(&bytes).unwrap();
    manifest.validate().unwrap();
    assert_eq!(manifest.package_root, fixture.package);
    assert_eq!(manifest.symbol_hash_protocol, "cpython-ast-dump-3.12");
    for key in COMPONENTS.iter().chain(BINDINGS.iter()) {
        let pin = manifest
            .components
            .get(*key)
            .or_else(|| manifest.bindings.get(*key))
            .unwrap();
        assert_eq!(pin.path, fixture.package.join(key));
        assert_eq!(
            pin.sha256,
            format!("{:x}", Sha256::digest(fs::read(&pin.path).unwrap()))
        );
    }
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let mut keys: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bindings",
            "components",
            "external_invariant_probe",
            "lineage",
            "package_root",
            "schema_version",
            "symbol_hash_protocol"
        ]
    );
    assert!(value["lineage"].is_null() && value["external_invariant_probe"].is_null());
    assert_eq!(
        fs::metadata(&fixture.output).unwrap().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        fs::metadata(fixture.output.parent().unwrap())
            .unwrap()
            .mode()
            & 0o7777,
        0o700
    );
}

#[test]
fn runner_bundle_optional_lineage_and_native_probe_are_byte_pins() {
    let fixture = Fixture::new();
    let lineage = fixture
        .package
        .join("conductor/mutation_runner_lineage.json");
    let probe = fixture.package.join("bin/invariant-probe");
    write(&lineage, b"{\"transitions\":[]}\n", 0o600);
    write(
        &probe,
        b"\x7fELFtest shared library fixture, never loaded",
        0o644,
    );
    let mut request = fixture.request();
    request.lineage = Some(&lineage);
    request.external_invariant_probe = Some(&probe);
    generate(request).unwrap();
    let manifest: Manifest = serde_json::from_slice(&fs::read(&fixture.output).unwrap()).unwrap();
    assert_eq!(
        manifest.lineage.unwrap().sha256,
        format!("{:x}", Sha256::digest(fs::read(lineage).unwrap()))
    );
    assert_eq!(
        manifest.external_invariant_probe.unwrap().sha256,
        format!("{:x}", Sha256::digest(fs::read(probe).unwrap()))
    );
}

#[test]
fn runner_bundle_existing_output_and_symlink_are_never_overwritten() {
    let fixture = Fixture::new();
    write(&fixture.output, b"sentinel", 0o600);
    assert!(generate(fixture.request()).is_err());
    assert_eq!(fs::read(&fixture.output).unwrap(), b"sentinel");
    fs::remove_file(&fixture.output).unwrap();
    let target = fixture.package.join("sentinel");
    write(&target, b"target", 0o600);
    symlink(&target, &fixture.output).unwrap();
    assert!(generate(fixture.request()).is_err());
    assert!(fs::symlink_metadata(&fixture.output)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(target).unwrap(), b"target");
}

#[test]
fn runner_bundle_missing_symlinked_and_hardlinked_inputs_fail_closed() {
    let fixture = Fixture::new();
    let path = fixture.package.join(COMPONENTS[0]);
    let saved = fixture.package.join("saved");
    fs::rename(&path, &saved).unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
    symlink(&saved, &path).unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
    fs::remove_file(&path).unwrap();
    fs::hard_link(&saved, &path).unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
    fs::remove_file(&path).unwrap();
    fs::rename(&saved, &path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
}

#[test]
fn runner_bundle_enforces_source_file_probe_and_aggregate_bounds() {
    let fixture = Fixture::new();
    let path = fixture.package.join(COMPONENTS[0]);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len((MAX_SOURCE_BYTES + 1) as u64)
        .unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
    for key in COMPONENTS.iter().chain(BINDINGS.iter()) {
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.package.join(key))
            .unwrap()
            .set_len(MAX_SOURCE_BYTES as u64)
            .unwrap();
    }
    // Exactly32MiB of sources leaves no room for the manifest itself.
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
    for key in COMPONENTS.iter().chain(BINDINGS.iter()) {
        write(&fixture.package.join(key), b"small", 0o600);
    }
    let probe = fixture.package.join("probe");
    write(&probe, b"\x7fELF", 0o700);
    fs::OpenOptions::new()
        .write(true)
        .open(&probe)
        .unwrap()
        .set_len((MAX_PROBE_BYTES + 1) as u64)
        .unwrap();
    let mut request = fixture.request();
    request.external_invariant_probe = Some(&probe);
    assert!(generate(request).is_err());
    fixture.no_output();
}

#[test]
fn runner_bundle_refuses_interpreted_probe_and_outside_optional_file() {
    let fixture = Fixture::new();
    let probe = fixture.package.join("probe");
    write(&probe, b"#!/usr/bin/python3\nraise SystemExit(0)\n", 0o700);
    let mut request = fixture.request();
    request.external_invariant_probe = Some(&probe);
    assert!(generate(request).is_err());
    fixture.no_output();
    let outside = fixture.repo.parent().unwrap().join("outside.json");
    write(&outside, b"{}", 0o600);
    let mut request = fixture.request();
    request.lineage = Some(&outside);
    assert!(generate(request).is_err());
    fixture.no_output();
}

#[test]
fn runner_bundle_excludes_source_git_snapshot_scratch_and_unsafe_output_parent() {
    let fixture = Fixture::new();
    let git = fixture.repo.join(".git");
    for root in [
        fixture.repo.as_path(),
        git.as_path(),
        fixture.repo.parent().unwrap(),
    ] {
        let mut request = fixture.request();
        request.package_root = root;
        assert!(generate(request).is_err());
    }
    let roots = vec![fixture.package.clone()];
    let mut request = fixture.request();
    request.exclude_roots = &roots;
    assert!(generate(request).is_err());
    fixture.no_output();
    let mut request = fixture.request();
    let inside = fixture.package.join("bundle.json");
    request.output = &inside;
    assert!(generate(request).is_err());
    assert!(!inside.exists());
    fs::set_permissions(
        fixture.output.parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(generate(fixture.request()).is_err());
    fixture.no_output();
}

#[test]
fn runner_bundle_requires_canonical_package_and_shared_schema_inventory() {
    let fixture = Fixture::new();
    let alias = fixture.package.parent().unwrap().join("package-alias");
    symlink(&fixture.package, &alias).unwrap();
    let mut request = fixture.request();
    request.package_root = &alias;
    assert!(generate(request).is_err());
    fixture.no_output();
    generate(fixture.request()).unwrap();
    let bytes = fs::read(&fixture.output).unwrap();
    let mut manifest: Manifest = serde_json::from_slice(&bytes).unwrap();
    manifest.components.remove(COMPONENTS[0]);
    assert!(manifest.validate().is_err());
    let mut manifest: Manifest = serde_json::from_slice(&bytes).unwrap();
    manifest.symbol_hash_protocol = "cpython-ast-dump-3.11".into();
    assert!(manifest.validate().is_err());
    let mut manifest: Manifest = serde_json::from_slice(&bytes).unwrap();
    manifest.schema_version = 2;
    assert!(manifest.validate().is_err());
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["execution_verified"] = Value::Bool(true);
    assert!(serde_json::from_value::<Manifest>(value).is_err());
}

#[test]
fn runner_bundle_rechecks_captured_bytes_and_cleans_failed_publication() {
    let fixture = Fixture::new();
    let path = fixture.package.join(COMPONENTS[0]);
    let mut captured = Captured::new(&fixture.package, &fixture.repo).unwrap();
    captured.pin(&path, MAX_SOURCE_BYTES, false).unwrap();
    write(&path, b"changed installed bytes", 0o600);
    assert!(captured.verify().is_err());
    let destination = output::Destination::open(&fixture.output).unwrap();
    assert!(destination
        .publish(b"complete", || captured.verify())
        .is_err());
    fixture.no_output();
}

#[test]
fn runner_bundle_atomic_publish_refuses_a_destination_created_after_open() {
    let fixture = Fixture::new();
    let destination = output::Destination::open(&fixture.output).unwrap();
    let result = destination.publish(b"complete", || {
        write(&fixture.output, b"concurrent sentinel", 0o600);
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&fixture.output).unwrap(), b"concurrent sentinel");
    assert_eq!(
        fs::read_dir(fixture.output.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}
