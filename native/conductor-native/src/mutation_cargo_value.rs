//! Validation of generated Cargo value metadata against independently held inputs.
//! A valid envelope is not execution authority or evidence of producer identity.
use crate::mutation_cargo_inventory::Inventory;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    declaration_path: String,
    declaration_sha256: String,
    declaration: Value,
    inventory: Value,
}
fn envelope(payload: &Value) -> Result<Option<Envelope>, String> {
    let object = payload.as_object().ok_or("campaign must be an object")?;
    match object.get("generated_value") {
        None => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| format!("invalid generated_value: {e}")),
    }
}
/// Extract the canonical declaration path without granting the envelope authority.
pub fn declaration_path(payload: &Value) -> Result<Option<String>, String> {
    let Some(envelope) = envelope(payload)? else {
        return Ok(None);
    };
    let path =
        crate::mutation_manifest::safe_relative(&envelope.declaration_path, "declaration path")?;
    if path != envelope.declaration_path || path.contains('\\') || path.len() > 4096 {
        return Err("declaration path must be canonical".into());
    }
    Ok(Some(path))
}
fn validate_generator(
    payload: &Value,
    selected: &BTreeSet<&str>,
    target: &crate::mutation_cargo_inventory::Target,
) -> Result<(), String> {
    let generator = payload["generator"]
        .as_object()
        .ok_or("generator missing")?;
    if generator.keys().any(|key| {
        ![
            "source",
            "exclude",
            "operators",
            "options",
            "seed",
            "jobs",
            "run_timeout_seconds",
        ]
        .contains(&key.as_str())
    }) {
        return Err("unsupported generator field".into());
    }
    if generator.get("jobs").and_then(Value::as_u64) != Some(1) {
        return Err("generated Cargo value requires exactly one worker".into());
    }
    let sources = generator
        .get("source")
        .and_then(Value::as_array)
        .ok_or("generator source missing")?;
    if sources.len() != selected.len()
        || sources
            .iter()
            .any(|v| !v.as_str().is_some_and(|s| selected.contains(s)))
        || sources
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>()
            .len()
            != selected.len()
    {
        return Err("generator sources differ from selected sources".into());
    }
    if generator.get("exclude") != Some(&json!([]))
        || generator.get("operators") != Some(&json!([]))
        || generator.get("options")
            != Some(
                &json!({"manifest_path":"Cargo.toml","package":target.package(),"package_root":""}),
            )
        || payload["environment"] != json!({})
    {
        return Err("unsupported generator options or environment".into());
    }
    Ok(())
}
/// Validate the whole campaign against original declaration bytes and an independently rebuilt inventory.
pub fn validate(
    payload: &Value,
    declaration: &[u8],
    selected_sources: &[String],
    inventory: &Inventory,
) -> Result<(), String> {
    let env = envelope(payload)?.ok_or("generated_value is absent")?;
    declaration_path(payload)?;
    if env.schema_version != 1 {
        return Err("unsupported generated_value schema".into());
    }
    let parsed = crate::mutation_rust_value_contract::parse(declaration, selected_sources)?;
    let original: Value = serde_json::from_slice(declaration).map_err(|e| e.to_string())?;
    let declaration_hash = format!("{:x}", Sha256::digest(declaration));
    if original != env.declaration
        || env.declaration_sha256 != declaration_hash
        || inventory.declaration_sha256() != declaration_hash
        || env.inventory != inventory.to_value()?
    {
        return Err("declaration or inventory differs from held input".into());
    }
    if payload["language"] != "rust" || payload["mutation_engine"] != "cargo-mutants" {
        return Err("generated value requires Rust cargo-mutants".into());
    }
    let target = inventory.target();
    let argv = json!([
        "cargo",
        "test",
        "--manifest-path",
        "Cargo.toml",
        "--package",
        target.package(),
        "--test",
        target.test(),
        "--locked",
        "--offline"
    ]);
    if payload["test_argv"] != argv {
        return Err("generated Cargo test command differs from target".into());
    }
    let tests = payload["test_sha256"]
        .as_object()
        .ok_or("test_sha256 missing")?;
    if tests.len() != 1
        || tests.get(target.path()).and_then(Value::as_str)
            != inventory.files().get(target.path()).map(String::as_str)
    {
        return Err("test hash differs from target inventory".into());
    }
    let source_map = payload["source_sha256"]
        .as_object()
        .ok_or("source_sha256 missing")?;
    let selected: BTreeSet<_> = selected_sources.iter().map(String::as_str).collect();
    if source_map.len() != selected.len()
        || source_map.iter().any(|(p, h)| {
            !selected.contains(p.as_str())
                || h.as_str() != inventory.files().get(p).map(String::as_str)
        })
    {
        return Err("source hashes differ from inventory".into());
    }
    validate_generator(payload, &selected, target)?;
    if parsed.target().test() != target.test()
        || parsed.target().package() != target.package()
        || parsed.target().manifest_path() != target.manifest_path()
        || parsed
            .ranked_tests()
            .iter()
            .map(|row| row.nodeid())
            .ne(inventory.ranked_nodeids().iter().map(String::as_str))
    {
        return Err("declaration target or ranked tests mismatch".into());
    }
    Ok(())
}
pub(crate) fn apply(
    contract: &mut crate::mutation_manifest::CampaignContract,
    payload: &Value,
    declaration: &[u8],
    inventory: &Inventory,
) -> Result<(), String> {
    let sources: Vec<String> = contract
        .source_sha256
        .as_object()
        .ok_or("source map missing")?
        .keys()
        .cloned()
        .collect();
    validate(payload, declaration, &sources, inventory)?;
    let parsed = crate::mutation_rust_value_contract::parse(declaration, &sources)?;
    contract.ranked_tests = parsed
        .ranked_tests()
        .iter()
        .map(|row| crate::mutation_manifest::RankedTestContract {
            rank: row.rank() as i64,
            nodeid: row.nodeid().into(),
            contract: row.contract().into(),
            rationale: row.rationale().into(),
        })
        .collect();
    let expected_nodeids = parsed
        .ranked_tests()
        .iter()
        .map(|row| row.nodeid().to_owned())
        .collect();
    contract.value_analysis = Some(crate::mutation_manifest::ValueContract {
        expected_nodeids,
        expected_repetitions: u64::from(parsed.baseline_repetitions()),
    });
    contract.generated_value = payload.get("generated_value").cloned();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation_cargo_inventory::{build, Entry};
    use std::collections::BTreeMap;
    fn file(value: &str) -> Entry {
        Entry::Regular(Some(value.as_bytes().to_vec()))
    }
    fn fixture() -> (Value, Vec<u8>, Vec<String>, Inventory) {
        let declaration = json!({"schema_version":1,"target":{"manifest_path":"Cargo.toml","package":"demo","test":"contract"},"baseline_repetitions":2,
        "required_contracts":[{"id":"c","criticality":"high","active_paths":["src/a.rs"]}],"ranked_tests":[{"rank":1,"nodeid":"tests/contract.rs::check","contract":"c","rationale":"behavior"}]}).to_string().into_bytes();
        let sources = vec!["src/a.rs".to_owned()];
        let entries = BTreeMap::from([
            ("Cargo.toml".into(), file("[package]\nname='demo'\nversion='0.1.0'\n[[test]]\nname='contract'\npath='tests/contract.rs'")),
            ("Cargo.lock".into(), file("version=4\n[[package]]\nname='demo'\nversion='0.1.0'")),
            ("src".into(), Entry::Directory), ("tests".into(), Entry::Directory),
            ("src/lib.rs".into(), file("pub mod a;")), ("src/a.rs".into(), file("pub fn a() {}")),
            ("tests/contract.rs".into(), file("#[test] fn check() { assert!(true); }")),
        ]);
        let inventory = build(&declaration, &sources, &entries).unwrap();
        let payload = json!({"language":"rust","mutation_engine":"cargo-mutants","test_argv":["cargo","test","--manifest-path","Cargo.toml","--package","demo","--test","contract","--locked","--offline"],"environment":{},
            "generator":{"source":sources,"exclude":[],"operators":[],"jobs":1,"options":{"manifest_path":"Cargo.toml","package":"demo","package_root":""}},
            "source_sha256":{"src/a.rs":inventory.files()["src/a.rs"]},"test_sha256":{"tests/contract.rs":inventory.files()["tests/contract.rs"]},
            "generated_value":{"schema_version":1,"declaration_path":"contract.json","declaration_sha256":format!("{:x}", Sha256::digest(&declaration)),"declaration":serde_json::from_slice::<Value>(&declaration).unwrap(),"inventory":inventory.to_value().unwrap()}});
        (payload, declaration, sources, inventory)
    }
    #[test]
    fn binds_declaration_inventory_and_exact_command() {
        let (payload, declaration, sources, inventory) = fixture();
        assert_eq!(
            declaration_path(&payload).unwrap().as_deref(),
            Some("contract.json")
        );
        validate(&payload, &declaration, &sources, &inventory).unwrap();
        for (field, value) in [
            ("test_argv", json!(["cargo", "test"])),
            ("environment", json!({"RUSTFLAGS":"-Z"})),
            ("mutation_engine", json!("fest")),
        ] {
            let mut changed = payload.clone();
            changed[field] = value;
            assert!(
                validate(&changed, &declaration, &sources, &inventory).is_err(),
                "{field}"
            );
        }
        for jobs in [Value::Null, json!("1"), json!(4)] {
            let mut changed = payload.clone();
            changed["generator"]["jobs"] = jobs;
            assert!(validate(&changed, &declaration, &sources, &inventory).is_err());
        }
        let mut changed = payload.clone();
        changed["generated_value"]["declaration"]["baseline_repetitions"] = json!(3);
        assert!(validate(&changed, &declaration, &sources, &inventory).is_err());
        let mut other_declaration: Value = serde_json::from_slice(&declaration).unwrap();
        other_declaration["baseline_repetitions"] = json!(3);
        let other_bytes = serde_json::to_vec(&other_declaration).unwrap();
        changed = payload.clone();
        changed["generated_value"]["declaration"] = other_declaration;
        changed["generated_value"]["declaration_sha256"] =
            json!(format!("{:x}", Sha256::digest(&other_bytes)));
        assert!(validate(&changed, &other_bytes, &sources, &inventory).is_err());
        changed = payload.clone();
        changed["generated_value"] = Value::Null;
        assert!(declaration_path(&changed).is_err());
    }
    #[test]
    fn captured_loader_requires_independent_inventory_and_held_declaration() {
        let (mut payload, declaration, sources, inventory) = fixture();
        payload["schema_version"] = json!(1);
        payload["campaign_id"] = json!("test");
        payload["title"] = json!("test");
        payload["survivor_baseline"] = json!([]);
        payload["generator"]["run_timeout_seconds"] = json!(60);
        let mut files = BTreeMap::from([
            (
                "campaign.json".to_owned(),
                serde_json::to_vec(&payload).unwrap(),
            ),
            ("contract.json".to_owned(), declaration),
            ("src/a.rs".to_owned(), b"pub fn a() {}".to_vec()),
            (
                "tests/contract.rs".to_owned(),
                b"#[test] fn check() { assert!(true); }".to_vec(),
            ),
        ]);
        assert!(
            crate::mutation_evidence::load_campaign_captured("campaign.json", &files, &[]).is_err()
        );
        let loaded = crate::mutation_evidence::load_campaign_captured_with_cargo(
            "campaign.json",
            &files,
            &[],
            &inventory,
        )
        .unwrap();
        assert_eq!(
            loaded["ranked_tests"][0]["nodeid"],
            "tests/contract.rs::check"
        );
        assert_eq!(loaded["value_analysis"]["expected_repetitions"], 2);
        files.insert("contract.json".into(), b"{}".to_vec());
        assert!(crate::mutation_evidence::load_campaign_captured_with_cargo(
            "campaign.json",
            &files,
            &[],
            &inventory
        )
        .is_err());
        assert_eq!(sources.len(), 1);
    }
}
