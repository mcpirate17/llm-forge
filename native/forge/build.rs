//! Stamp the git rev into the binary so `forge --version` can tell two
//! builds apart (`forge hooks status` compares an installed hook's binary
//! against the running one by exactly this line). `unknown` when git is
//! absent or the tree is not a checkout -- never a build failure.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // .git/HEAD changes on every checkout and commit, so the rev follows
    // the tree instead of freezing at the first build. In a build without
    // a checkout (e.g. `cargo install` from a tarball) the path is absent
    // and is simply not tracked.
    if Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|rev| !rev.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FORGE_GIT_REV={rev}");
}
