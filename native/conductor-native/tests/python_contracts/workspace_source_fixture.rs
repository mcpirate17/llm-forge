//! Restore original Python hook source inputs for AST-inspecting workspace checks.

use crate::support::{module, text};
use pyo3::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn source(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/conductor/testdata/workspace_hooks")
        .join(name)
}

fn write_executable(target: &Path, contents: &str) {
    fs::remove_file(target).expect("remove native hook symlink");
    fs::write(target, contents).expect("write Python hook source input");
    let mut permissions = fs::metadata(target).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(target, permissions).expect("mark Python hook source executable");
}

pub fn install(py: Python<'_>, root: &Path) {
    let interpreter = text(&module(py, "sys").getattr("executable").unwrap());
    let gate = fs::read_to_string(source("crg_gate.py")).expect("read original gate source");
    write_executable(&root.join(".agent_hooks/crg_gate.py"), &gate);

    let obsidian = fs::read_to_string(source("obsidian_sync.py"))
        .expect("read original Obsidian no-op source");
    let obsidian = obsidian.replace("{python}", &interpreter);
    for agent in ["codex", "claude"] {
        write_executable(
            &root.join(format!(".{agent}/hooks/obsidian_sync.py")),
            &obsidian,
        );
    }
}
