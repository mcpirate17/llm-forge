//! Stamp the git rev into the binary so `forge --version` can tell two
//! builds apart (`forge hooks status` compares an installed hook's binary
//! against the running one by exactly this line). `unknown` when git is
//! absent or the tree is not a checkout -- never a build failure.

use std::path::Path;
use std::process::Command;
#[path = "../forge/src/source_fingerprint.rs"]
mod source_fingerprint;

fn git_text(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn watch_git_path(name: &str) {
    if let Some(path) = git_text(&["rev-parse", "--git-path", name]) {
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // HEAD names the branch; commits and fast-forwards update its ref instead.
    // Resolve Git paths so packed refs and git-dir indirection also work.
    for name in ["HEAD", "packed-refs"] {
        watch_git_path(name);
    }
    if let Some(branch) = git_text(&["symbolic-ref", "--quiet", "HEAD"]) {
        watch_git_path(&branch);
    }
    let rev = git_text(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=FORGE_GIT_REV={rev}");
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"),
    );
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("native repository layout");
    let files =
        source_fingerprint::files(root).expect("inventory native source for build fingerprint");
    for path in &files {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    // Watching the source directories also detects newly added/deleted modules.
    for name in ["forge", "conductor-native", "slop-core"] {
        println!(
            "cargo:rerun-if-changed={}",
            root.join("native").join(name).join("src").display()
        );
    }
    let fingerprint = source_fingerprint::fingerprint(root, &files)
        .expect("hash native source for build fingerprint");
    println!("cargo:rustc-env=FORGE_SOURCE_SHA256={fingerprint}");
}
