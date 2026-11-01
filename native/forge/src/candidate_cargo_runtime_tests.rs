use super::*;
use std::ffi::OsStr;
use std::os::unix::fs::symlink;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::resource_id("cargo-runtime-test").unwrap());
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn tool_validation_rejects_scripts_symlinks_and_missing() {
    let f = Fixture::new();
    let bin = f.dir("bin");
    fs::write(bin.join("cargo"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(tool(&bin, "cargo").is_err());
    fs::write(bin.join("real"), b"\x7fELF\0fixture").unwrap();
    fs::set_permissions(bin.join("real"), fs::Permissions::from_mode(0o700)).unwrap();
    symlink("real", bin.join("rustc")).unwrap();
    assert!(tool(&bin, "rustc").is_err());
    assert!(tool(&bin, "missing").is_err());
    assert!(tool(&bin, "real").is_ok());
    fs::set_permissions(bin.join("real"), fs::Permissions::from_mode(0o720)).unwrap();
    assert!(tool(&bin, "real").is_err());
}
#[test]
fn cache_copy_limits_scope_and_never_hardlinks() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let seed = f.dir("seed");
    let target = f.dir("target");
    fs::create_dir_all(seed.join("registry/cache/nested")).unwrap();
    fs::write(seed.join("registry/cache/nested/pkg"), b"contents").unwrap();
    fs::write(seed.join("credentials.toml"), b"secret").unwrap();
    cache::copy_seed(&seed, &target).unwrap();
    let old = fs::metadata(seed.join("registry/cache/nested/pkg")).unwrap();
    let new = fs::metadata(target.join("registry/cache/nested/pkg")).unwrap();
    assert_ne!((old.dev(), old.ino()), (new.dev(), new.ino()));
    assert_eq!(
        fs::read(target.join("registry/cache/nested/pkg")).unwrap(),
        b"contents"
    );
    assert!(!target.join("credentials.toml").exists());
    fs::write(target.join("registry/cache/nested/pkg"), b"changed").unwrap();
    assert_eq!(
        fs::read(seed.join("registry/cache/nested/pkg")).unwrap(),
        b"contents"
    );
}
#[test]
fn cache_refuses_symlink_and_cwd_rejects_config() {
    let f = Fixture::new();
    let seed = f.dir("seed");
    let target = f.dir("target");
    fs::create_dir_all(seed.join("registry/cache")).unwrap();
    symlink("/etc/passwd", seed.join("registry/cache/link")).unwrap();
    assert!(cache::copy_seed(&seed, &target).is_err());
    let cwd = f.dir("safe-cwd/inner");
    fs::create_dir(f.0.join("safe-cwd/.cargo")).unwrap();
    fs::write(f.0.join("safe-cwd/.cargo/config.toml"), b"[build]\n").unwrap();
    assert!(clean_cwd(&cwd).is_err());
}

#[test]
fn cache_rejects_oversized_file_before_copy() {
    let f = Fixture::new();
    let seed = f.dir("seed");
    let target = f.dir("target");
    fs::create_dir_all(seed.join("registry/cache")).unwrap();
    fs::File::create(seed.join("registry/cache/oversized"))
        .unwrap()
        .set_len(256 * 1024 * 1024 + 1)
        .unwrap();
    assert!(cache::copy_seed(&seed, &target).is_err());
    assert!(!target.join("registry/cache/oversized").exists());
}
#[test]
fn command_binds_private_target_and_preserves_no_host_mutation() {
    let f = Fixture::new();
    let candidate = f.dir("candidate");
    let tmp = f.dir("tmp");
    let bin = f.dir("bin");
    fs::write(candidate.join("Cargo.toml"), b"[package]\n").unwrap();
    let mut state = State::new(&tmp).unwrap();
    state.initialize().unwrap();
    let path = state.path();
    let runtime = Runtime {
        candidate: candidate.clone(),
        bin: bin.clone(),
        paths: vec![],
        state,
        mode: Mode::Fmt,
    };
    let command = runtime
        .command(Some(&candidate.join("Cargo.toml")))
        .unwrap();
    assert_eq!(command.get_program(), bin.join("cargo"));
    assert_eq!(command.get_current_dir().unwrap(), path.join("cwd"));
    let values: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_os_string(), v.to_os_string())))
        .collect();
    assert_eq!(values.get(OsStr::new("PATH")).unwrap(), bin.as_os_str());
    assert_eq!(
        values.get(OsStr::new("CARGO_HOME")).unwrap(),
        path.join("cargo-home").as_os_str()
    );
    assert!(
        Path::new(values.get(OsStr::new("CARGO_TARGET_DIR")).unwrap())
            .starts_with(path.join("targets"))
    );
    let target = PathBuf::from(values.get(OsStr::new("CARGO_TARGET_DIR")).unwrap());
    fs::remove_dir(&target).unwrap();
    symlink(&candidate, &target).unwrap();
    assert!(runtime
        .command(Some(&candidate.join("Cargo.toml")))
        .is_err());
    assert!(runtime
        .command(Some(&f.0.join("elsewhere/Cargo.toml")))
        .is_err());
    runtime.finish().unwrap();
    assert!(!path.exists());
}

#[test]
fn explicit_finish_surfaces_replaced_state_and_preserves_sentinel() {
    let f = Fixture::new();
    let candidate = f.dir("candidate");
    let tmp = f.dir("tmp");
    let bin = f.dir("bin");
    let state = State::new(&tmp).unwrap();
    let original = state.path();
    let moved = tmp.join("original-moved");
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("sentinel"), b"keep").unwrap();
    let runtime = Runtime {
        candidate,
        bin,
        paths: vec![],
        state,
        mode: Mode::Fmt,
    };
    assert!(runtime.finish().is_err());
    assert_eq!(fs::read(original.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn stage_scratch_rule_does_not_reject_operator_siblings() {
    let parent = Path::new("/work/fixture");
    let candidate = parent.join("candidate");
    let sibling = parent.join("toolchain");
    assert!(exclusive(&sibling, &candidate, &parent.join("tmp")).is_ok());
    assert!(exclusive(&sibling, &candidate, &parent.join("runtime/tmp")).is_err());
    let staged = parent.join(".forge-candidate-1-0123456789abcdef");
    assert!(exclusive(&sibling, &staged, &parent.join("runtime/tmp")).is_err());
    assert!(exclusive(&candidate.join("bin"), &candidate, &parent.join("tmp")).is_err());
}

#[test]
fn command_rejects_replaced_layout_and_private_cargo_config() {
    for poison in [
        "cwd",
        "cargo-home",
        "targets",
        "cwd-dir",
        "home-dir",
        "targets-dir",
        "config",
        "config.toml",
    ] {
        let f = Fixture::new();
        let candidate = f.dir("candidate");
        let tmp = f.dir("tmp");
        let bin = f.dir("bin");
        fs::write(candidate.join("Cargo.toml"), b"[package]\n").unwrap();
        let mut state = State::new(&tmp).unwrap();
        state.initialize().unwrap();
        let path = state.path();
        if poison.starts_with("config") {
            fs::write(path.join("cargo-home").join(poison), b"[build]\n").unwrap();
        } else if poison.ends_with("-dir") {
            let name = match poison {
                "home-dir" => "cargo-home",
                "cwd-dir" => "cwd",
                _ => "targets",
            };
            fs::rename(path.join(name), path.join(format!("{name}-old"))).unwrap();
            fs::create_dir(path.join(name)).unwrap();
        } else {
            fs::rename(path.join(poison), path.join(format!("{poison}-old"))).unwrap();
            symlink(&candidate, path.join(poison)).unwrap();
        }
        let runtime = Runtime {
            candidate: candidate.clone(),
            bin,
            paths: vec![],
            state,
            mode: Mode::Fmt,
        };
        assert!(
            runtime
                .command(Some(&candidate.join("Cargo.toml")))
                .is_err(),
            "{poison}"
        );
        runtime.finish().unwrap();
    }
}
