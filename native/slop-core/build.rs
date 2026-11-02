//! Permanent source-stamp sentinel; no custom build work is required.
//! Keeping this named input present lets Cargo cache the CLI stamp: a missing
//! `rerun-if-changed` path is dirty on every invocation.
fn main() {
    // Avoid Cargo's default package-wide scan for an otherwise empty script.
    println!("cargo:rerun-if-changed=build.rs");
}
