//! Exercise Cargo's real freshness decisions with the production stamp sources.
//! The fixture compiles a tiny executable, not another copy of Forge.
use std::{fs, path::PathBuf, process::Command, time::SystemTime};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove build-stamp fixture");
    }
}
impl Fixture {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let fixture =
            Self(std::env::temp_dir().join(format!("forge-stamp-{}-{unique}", std::process::id())));
        for name in ["forge", "conductor-native", "slop-core"] {
            fixture.write(
                &format!("native/{name}/Cargo.toml"),
                &format!("[package]\nname='{name}'\n"),
            );
            fixture.write(&format!("native/{name}/Cargo.lock"), "version = 4\n");
            fixture.write(
                &format!("native/{name}/src/lib.rs"),
                "pub fn fixture() {}\n",
            );
        }
        for (name, body) in [
            ("forge", include_str!("../build.rs")),
            (
                "conductor-native",
                include_str!("../../conductor-native/build.rs"),
            ),
            ("slop-core", include_str!("../../slop-core/build.rs")),
        ] {
            fixture.write(&format!("native/{name}/build.rs"), body);
        }
        fixture.write(
            "native/forge-cli/build.rs",
            include_str!("../../forge-cli/build.rs"),
        );
        fixture.write(
            "native/forge-cli/source_identity.rs",
            include_str!("../../forge-cli/source_identity.rs"),
        );
        fixture.write(
            "native/forge/src/source_fingerprint.rs",
            include_str!("../src/source_fingerprint.rs"),
        );
        fixture.write(
            "native/conductor-native/src/source_fingerprint.rs",
            include_str!("../../conductor-native/src/source_fingerprint.rs"),
        );
        fixture.write("src/conductor/memory_sources.toml", "fixture\n");
        fixture.write("src/conductor/embedding_routes.toml", "fixture\n");
        fixture.write("native/forge-cli/Cargo.toml", "[package]\nname='stamp-fixture'\nversion='0.0.0'\nedition='2021'\n[workspace]\n[[bin]]\nname='stamp-fixture'\npath='../forge/src/main.rs'\n[build-dependencies]\nsha1='0.10'\nsha2='0.10'\nlibc='0.2'\n");
        fixture.write("native/forge/src/main.rs", "fn main() { println!(\"{} {} {}\", env!(\"FORGE_GIT_FULL_REV\"), env!(\"FORGE_SOURCE_SHA256\"), env!(\"FORGE_SOURCE_MATCHES_GIT\")); }\n");
        fixture.write("ledger/routing_policy.toml", "fixture\n");
        fixture.write(
            "native/forge/tests/fixtures/ledger/calibration.json",
            "fixture\n",
        );
        fixture.write(
            ".gitignore",
            "native/forge/target/\nnative/forge-cli/Cargo.lock\n",
        );
        fixture.git(&["init", "-b", "fixture"]);
        fixture.commit();
        fixture
    }
    fn write(&self, path: &str, body: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.0)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
    }
    fn build(&self, compiled: bool, clean: bool) -> Vec<String> {
        let target = self.0.join("native/forge/target");
        let output = Command::new("cargo")
            .current_dir(&self.0)
            .args([
                "+1.98.0",
                "build",
                "--offline",
                "--jobs",
                "1",
                "--manifest-path",
                "native/forge-cli/Cargo.toml",
                "--target-dir",
            ])
            .arg(&target)
            .arg("-v")
            .env("RUSTUP_TOOLCHAIN", "1.98.0")
            .env("CARGO_TERM_COLOR", "never")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "cargo: {stderr}");
        assert_eq!(
            stderr.contains("Compiling stamp-fixture"),
            compiled,
            "{stderr}"
        );
        let output = Command::new(target.join("debug/stamp-fixture"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let stamp: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        assert_eq!(stamp.len(), 3);
        assert_eq!(stamp[2], if clean { "1" } else { "0" });
        stamp
    }
}

#[test]
fn native_stamp_is_fresh_and_tracks_source_inventory_without_watching_caches() {
    let f = Fixture::new();
    let original = f.build(true, true);
    assert_eq!(f.build(false, true), original);
    f.write(
        "native/forge/target/arbitrary/deep/cache",
        "new cache bytes\n",
    );
    assert_eq!(f.build(false, true), original);
    f.write("native/forge/src/nested/new.rs", "new source\n");
    assert_ne!(f.build(true, false)[1], original[1]);
    fs::remove_dir_all(f.0.join("native/forge/src/nested")).unwrap();
    assert_eq!(f.build(true, true), original);
    fs::remove_file(f.0.join("native/slop-core/build.rs")).unwrap();
    assert_ne!(f.build(true, false)[1], original[1]);
    f.write(
        "native/slop-core/build.rs",
        include_str!("../../slop-core/build.rs"),
    );
    assert_eq!(f.build(true, true), original);
    for asset in [
        "src/conductor/memory_sources.toml",
        "src/conductor/embedding_routes.toml",
    ] {
        f.write(asset, "changed embedded asset\n");
        assert_ne!(f.build(true, false)[1], original[1]);
        f.write(asset, "fixture\n");
        assert_eq!(f.build(true, true), original);
    }
}

#[test]
fn native_stamp_tracks_loose_packed_and_detached_git_revisions() {
    let f = Fixture::new();
    let original = f.build(true, true);
    f.git(&["update-ref", "refs/remotes/origin/main", &original[0]]);
    assert_eq!(f.build(false, true), original);
    f.git(&["update-ref", "refs/forge/recovery/snapshot", &original[0]]);
    assert_eq!(f.build(false, true), original);
    f.write("outside.txt", "outside source inventory\n");
    f.commit();
    let next = f.build(true, true);
    assert_ne!(next[0], original[0]);
    assert_eq!(next[1], original[1]);
    f.git(&["pack-refs", "--all", "--prune"]);
    assert!(!f.0.join(".git/refs/heads/fixture").exists());
    assert_eq!(f.build(true, true), next);
    f.write("outside.txt", "another outside change\n");
    f.commit();
    let loose = f.build(true, true);
    assert_ne!(loose[0], next[0]);
    f.git(&["pack-refs", "--all", "--prune"]);
    assert_eq!(f.build(true, true), loose);
    let packed = f.0.join(".git/packed-refs");
    let text = fs::read_to_string(&packed).unwrap();
    fs::write(
        &packed,
        text.replace(
            &format!("{} refs/heads/fixture", loose[0]),
            &format!("{} refs/heads/fixture", original[0]),
        ),
    )
    .unwrap();
    assert_eq!(f.build(true, true), original);
    f.git(&["checkout", "--detach", &loose[0]]);
    assert_eq!(f.build(true, true), loose);
    assert_eq!(f.build(false, true), loose);
    f.git(&["checkout", "--orphan", "unborn"]);
    let unborn = f.build(true, false);
    assert_eq!(unborn[0], "unknown");
    assert_eq!(unborn[1], loose[1]);
    f.commit();
    let born = f.build(true, true);
    assert_ne!(born[0], "unknown");
    assert_eq!(f.build(false, true), born);
}
