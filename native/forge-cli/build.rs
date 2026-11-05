//! Stamp the git rev into the binary so `forge --version` can tell two
//! builds apart (`forge hooks status` compares an installed hook's binary
//! against the running one by exactly this line). `unknown` when git is
//! absent or the tree is not a checkout -- never a build failure.

use std::path::Path;
use std::process::Command;
#[path = "../conductor-native/src/source_fingerprint.rs"]
mod source_fingerprint;
#[path = "source_identity.rs"]
mod source_identity;

fn git_text(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn watch_git_path(name: &str, required: bool) {
    if let Some(path) = git_text(&["rev-parse", "--git-path", name]) {
        if required || Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

// Watch only the active branch namespace. Remote-tracking and Forge recovery
// refs do not change the build revision; watching all refs would relink on push.
fn watch_branch_parent(branch: &str) {
    let Some(path) = git_text(&["rev-parse", "--git-path", branch]) else {
        return;
    };
    let Some(refs) = git_text(&["rev-parse", "--git-path", "refs"]) else {
        return;
    };
    let path = Path::new(&path);
    let refs = Path::new(&refs);
    for directory in path.ancestors().skip(1) {
        if !directory.starts_with(refs) {
            break;
        }
        if directory.is_dir() {
            println!("cargo:rerun-if-changed={}", directory.display());
            return;
        }
    }
    // A missing refs namespace is conservatively dirty. Never fall back to the
    // Git directory itself: it contains object caches and unrelated metadata.
    println!("cargo:rerun-if-changed={}", refs.display());
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // HEAD names the branch; commits and fast-forwards update its ref instead.
    // Resolve Git paths so packed refs and git-dir indirection also work.
    watch_git_path("HEAD", true);
    watch_git_path("packed-refs", false);
    if let Some(branch) = git_text(&["symbolic-ref", "--quiet", "HEAD"]) {
        watch_git_path(&branch, false);
        // Packing can remove a loose ref; a later commit creates it again.
        // Watch its nearest existing parent to catch both packed and unborn refs.
        watch_branch_parent(&branch);
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
    let full_head = source_identity::revision(root);
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
        println!(
            "cargo:rerun-if-changed={}",
            root.join("native").join(name).join("build.rs").display()
        );
    }
    let fingerprint = source_fingerprint::fingerprint(root, &files)
        .expect("hash native source for build fingerprint");
    println!("cargo:rustc-env=FORGE_SOURCE_SHA256={fingerprint}");
    let clean = full_head
        .as_deref()
        .is_some_and(|head| source_identity::matches_head(root, head, &files, &fingerprint));
    println!(
        "cargo:rustc-env=FORGE_GIT_FULL_REV={}",
        full_head.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=FORGE_SOURCE_MATCHES_GIT={}",
        if clean { "1" } else { "0" }
    );
}
