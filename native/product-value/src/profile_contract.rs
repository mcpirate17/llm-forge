//! Admission and outcome contracts for diagnostics bound to completed paired data.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
pub const BLOB: &str = "4cd99821a4c6d9f9ddc4e9c3746f3dd0c4189edb";
pub const SOURCE_SHA: &str = "9015359e74c9fcd17a7bd898665ec6acbb610e088aa56b9e72defefebdf888cb";

pub struct Paired {
    pub source_bins: [PathBuf; 2],
    pub binding: Value,
}

pub fn validate(
    paired: &Value,
    native_sha: &str,
    python_sha: &str,
    alternate_build: Option<&Value>,
) -> Result<Paired> {
    ensure!(
        paired["schema"] == "forge-product-value-v1" && paired["complete"] == true,
        "profile requires complete forge-product-value-v1 paired evidence"
    );
    ensure!(
        paired["identity"]["python_elf_sha256"] == python_sha,
        "profile Python ELF differs from paired evidence"
    );
    ensure!(
        paired["identity"]["reference"]["git_blob"] == BLOB
            && paired["identity"]["reference"]["source_sha256"] == SOURCE_SHA,
        "paired retired Python reference identity differs from profiler reference"
    );
    let binding = runtime_binding(paired, native_sha, alternate_build)?;
    let cases = paired["relocation"]
        .as_array()
        .context("paired cases missing")?;
    ensure!(
        cases.len() == 2,
        "paired evidence must contain exactly two workload cases"
    );
    let mut bins = Vec::new();
    for (index, (scripts, body)) in [(16, 128), (256, 4096)].into_iter().enumerate() {
        let case = &cases[index];
        ensure!(
            case["scripts"] == scripts
                && case["body_bytes_per_script"] == body
                && case["complete"] == true,
            "paired workload {index} must be complete with {scripts} scripts and {body} body bytes"
        );
        let source = PathBuf::from(
            case["fixture"]["source_path"]
                .as_str()
                .context("paired source path missing")?,
        );
        let destination = PathBuf::from(
            case["fixture"]["destination_path"]
                .as_str()
                .context("paired destination path missing")?,
        );
        ensure!(
            source.is_absolute()
                && source.file_name().is_some_and(|n| n == "source-env")
                && destination == source.with_file_name("guest-env"),
            "paired fixture paths must identify sibling source-env/guest-env directories"
        );
        validate_fixture(&case["fixture"], &source, scripts, body)?;
        bins.push(source.join("bin"));
    }
    Ok(Paired {
        source_bins: bins.try_into().expect("two validated workloads"),
        binding,
    })
}

fn validate_fixture(
    fixture: &Value,
    source: &std::path::Path,
    scripts: usize,
    size: usize,
) -> Result<()> {
    let body = crate::fixture::body_bytes(size);
    ensure!(
        fixture["body_sha256"] == crate::fixture::hash(&body)
            && fixture["hardlinks_reset_before_each_candidate"] == true
            && fixture["script_modes"] == "0751"
            && fixture["shebangs"] == "alternating LF/CRLF; exact source Python3.12 alias"
            && fixture["symlink"] == json!({"alias":"script-0000"}),
        "paired fixture body hash or construction contract differs"
    );
    let image = fixture["source_image_dev_inode_mode_sha256"]
        .as_object()
        .context("paired source image missing")?;
    ensure!(
        image.len() == scripts + crate::fixture::SKIPPED.len(),
        "paired source entry inventory differs"
    );
    for (name, bytes) in crate::fixture::SKIPPED {
        validate_entry(
            image.get(name).context("paired skipped entry missing")?,
            &crate::fixture::hash(bytes),
            false,
        )?;
    }
    for index in 0..scripts {
        let name = format!("script-{index:04}");
        let hash = crate::fixture::hash(&crate::fixture::script_bytes(source, index, &body));
        validate_entry(
            image.get(&name).context("paired script entry missing")?,
            &hash,
            true,
        )?;
    }
    Ok(())
}

fn validate_entry(entry: &Value, expected_hash: &str, script: bool) -> Result<()> {
    let values = entry
        .as_array()
        .context("paired source metadata must be array")?;
    ensure!(
        values.len() == 4 && values[0].as_u64().is_some() && values[1].as_u64().is_some(),
        "paired source metadata must contain device/inode/mode/hash"
    );
    let mode = values[2].as_u64().context("paired source mode missing")?;
    ensure!(
        mode & 0o170000 == 0o100000
            && (!script || mode & 0o7777 == 0o751)
            && values[3] == expected_hash,
        "paired source mode/content differs"
    );
    Ok(())
}

fn runtime_binding(
    paired: &Value,
    native_sha: &str,
    alternate_build: Option<&Value>,
) -> Result<Value> {
    let original_native = paired["identity"]["forge_sha256"]
        .as_str()
        .context("paired native ELF identity missing")?;
    let binding = if original_native == native_sha {
        ensure!(
            alternate_build.is_none(),
            "alternate build supplied for unchanged native ELF"
        );
        json!({"mode":"same_paired_runtimes","paired_native_elf_sha256":original_native})
    } else {
        let build = alternate_build.context(
            "different native ELF requires explicit SHA-pinned --alternate-native-build-receipt",
        )?;
        let commit = build["candidate"]["commit"]
            .as_str()
            .context("alternate candidate commit missing")?;
        ensure!(
            build["schema"] == "forge-owned-build-observation-v1"
                && build["candidate"]["elf_sha256"] == native_sha
                && commit.len() == 40
                && commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "alternate build observation must identify selected candidate ELF and full commit"
        );
        json!({"mode":"alternate_native_diagnostic","paired_native_elf_sha256":original_native,
            "candidate_commit":commit,"build_observation":build,
            "scope":"operator-supplied build observation, not execution attestation; different native runtime, not a trace of the original paired native ELF"})
    };
    Ok(binding)
}

pub fn failure_status(timed_out: bool, cancelled: bool) -> &'static str {
    if timed_out || cancelled {
        "censored"
    } else {
        "failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paired() -> Value {
        let mut paired = json!({"schema":"forge-product-value-v1","complete":true,
            "identity":{"python_elf_sha256":"python","forge_sha256":"original"},
            "relocation":[
                {"scripts":16,"body_bytes_per_script":128,"complete":true,
                 "fixture":{"source_path":"/tmp/fixture-16/source-env","destination_path":"/tmp/fixture-16/guest-env"}},
                {"scripts":256,"body_bytes_per_script":4096,"complete":true,
                 "fixture":{"source_path":"/tmp/fixture-256/source-env","destination_path":"/tmp/fixture-256/guest-env"}}]});
        paired["identity"]["reference"] = json!({"git_blob":BLOB,"source_sha256":SOURCE_SHA});
        for case in paired["relocation"].as_array_mut().unwrap() {
            let scripts = case["scripts"].as_u64().unwrap() as usize;
            let body = crate::fixture::body_bytes(
                case["body_bytes_per_script"].as_u64().unwrap() as usize
            );
            let source = PathBuf::from(case["fixture"]["source_path"].as_str().unwrap());
            let mut image = serde_json::Map::new();
            for (name, bytes) in crate::fixture::SKIPPED {
                image.insert(
                    name.to_owned(),
                    json!([1, 2, 0o100644, crate::fixture::hash(bytes)]),
                );
            }
            for index in 0..scripts {
                image.insert(
                    format!("script-{index:04}"),
                    json!([
                        1,
                        2,
                        0o100751,
                        crate::fixture::hash(&crate::fixture::script_bytes(&source, index, &body))
                    ]),
                );
            }
            let fixture = &mut case["fixture"];
            fixture["body_sha256"] = json!(crate::fixture::hash(&body));
            fixture["hardlinks_reset_before_each_candidate"] = json!(true);
            fixture["script_modes"] = json!("0751");
            fixture["shebangs"] = json!("alternating LF/CRLF; exact source Python3.12 alias");
            fixture["symlink"] = json!({"alias":"script-0000"});
            fixture["source_image_dev_inode_mode_sha256"] = json!(image);
        }
        paired
    }
    #[test]
    fn rejects_incomplete_or_different_workload() {
        let mut data = paired();
        assert!(validate(&data, "original", "python", None).is_ok());
        data["complete"] = json!(false);
        assert!(validate(&data, "original", "python", None).is_err());
        data = paired();
        data["relocation"][1]["body_bytes_per_script"] = json!(128);
        assert!(validate(&data, "original", "python", None).is_err());
        data = paired();
        data["relocation"][0]["fixture"]["destination_path"] = json!("/tmp/unrelated");
        assert!(validate(&data, "original", "python", None).is_err());
    }
    #[test]
    fn requires_explicit_matching_alternate_build() {
        let data = paired();
        assert!(validate(&data, "original", "other-python", None).is_err());
        assert!(validate(&data, "candidate", "python", None).is_err());
        let mut build = json!({"schema":"forge-owned-build-observation-v1",
            "candidate":{"elf_sha256":"candidate","commit":"0123456789abcdef0123456789abcdef01234567"}});
        let accepted = validate(&data, "candidate", "python", Some(&build)).unwrap();
        assert_eq!(accepted.binding["mode"], "alternate_native_diagnostic");
        build["candidate"]["elf_sha256"] = json!("wrong");
        assert!(validate(&data, "candidate", "python", Some(&build)).is_err());
    }
    #[test]
    fn rejects_reference_and_fixture_content_drift() {
        for (pointer, replacement) in [
            ("/identity/reference/git_blob", json!("other-reference")),
            ("/identity/reference/source_sha256", json!("other-source")),
            ("/relocation/0/fixture/body_sha256", json!("other-body")),
            (
                "/relocation/0/fixture/hardlinks_reset_before_each_candidate",
                json!(false),
            ),
            (
                "/relocation/0/fixture/source_image_dev_inode_mode_sha256/script-0000/2",
                json!(0o100755),
            ),
            (
                "/relocation/0/fixture/source_image_dev_inode_mode_sha256/script-0000/3",
                json!("other-script"),
            ),
        ] {
            let mut data = paired();
            *data.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate(&data, "original", "python", None).is_err(),
                "accepted {pointer}"
            );
        }
    }
    #[test]
    fn retains_timeout_and_signal_censoring() {
        assert_eq!(failure_status(false, false), "failed");
        assert_eq!(failure_status(true, false), "censored");
        assert_eq!(failure_status(false, true), "censored");
        assert_eq!(failure_status(true, true), "censored");
    }
}
