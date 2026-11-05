//! Opt-in target-bound value metadata for automatically generated campaigns.
use crate::{mutation_campaign::sha256_hex, mutation_cargo_capture::Held};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

pub(crate) fn attach(
    result: &mut Value,
    root: &Path,
    declaration: &str,
    expected_sha256: &str,
) -> Result<Held> {
    ensure!(
        result["language"] == "rust",
        "Rust value declarations require Rust planning"
    );
    ensure!(
        expected_sha256.len() == 64
            && expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "declaration SHA-256 must be lowercase hexadecimal"
    );
    let manifests = result["manifests"]
        .as_array_mut()
        .context("planned manifests missing")?;
    ensure!(
        manifests.len() == 1,
        "Rust value declaration requires exactly one planned package"
    );
    let manifest = &mut manifests[0];
    ensure!(
        manifest["mutation_engine"] == "cargo-mutants",
        "Rust value evidence requires cargo-mutants"
    );
    ensure!(
        manifest["generator"]["jobs"] == 1,
        "Rust value evidence requires --jobs 1"
    );
    let sources: Vec<String> = manifest["source_sha256"]
        .as_object()
        .context("planned source hashes missing")?
        .keys()
        .cloned()
        .collect();
    let held = Held::capture(root, declaration, &sources)?;
    let bytes = held.declaration_bytes();
    ensure!(
        sha256_hex(bytes) == expected_sha256,
        "Rust value declaration digest mismatch"
    );
    let inventory = held.inventory();
    for source in &sources {
        ensure!(
            manifest["source_sha256"][source].as_str()
                == inventory.files().get(source).map(String::as_str),
            "planned source changed before Cargo inventory capture: {source}"
        );
    }
    let target = inventory.target();
    manifest["generated_value"] = json!({
        "schema_version": 1, "declaration_path": declaration,
        "declaration_sha256": expected_sha256,
        "declaration": serde_json::from_slice::<Value>(bytes)?,
        "inventory": inventory.to_value().map_err(anyhow::Error::msg)?,
    });
    manifest["test_argv"] = json!([
        "cargo",
        "test",
        "--manifest-path",
        target.manifest_path(),
        "--package",
        target.package(),
        "--test",
        target.test(),
        "--locked",
        "--offline"
    ]);
    manifest["test_sha256"] = json!({target.path(): inventory.files().get(target.path()).context("target hash missing")?});
    held.verify()?;
    Ok(held)
}
