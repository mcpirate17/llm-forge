//! Locked local A2A identity registry, liveness probes and guarded reaping.

use super::store::validate_identity;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

const MAX_REGISTRY_BYTES: u64 = 1_048_576;
const MAX_CARD_BYTES: u64 = 65_536;
const KNOWN_PORTS: [(&str, u16); 7] = [
    ("codex-phase22", 7310),
    ("glm-5.3", 7311),
    ("fable-nmf6", 7312),
    ("claude-opus-5", 7313),
    ("antigravity", 7314),
    ("fable-helm", 7315),
    ("grok", 7316),
];

#[derive(Clone)]
pub struct Record {
    pub name: String,
    pub port: u16,
    pub token: String,
    pub generation: String,
}

impl Record {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(format!("{}\0{}\0{}", self.name, self.port, self.generation));
        format!("{:x}", hash.finalize())
    }
}

struct RegistryLock(File);

impl RegistryLock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700))?;
        let path = state_dir.join(".registry.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("opening registry lock {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error()).context("locking A2A registry");
        }
        Ok(Self(file))
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn read_json(path: &Path, label: &str) -> Result<Option<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("opening {label} {}", path.display()))
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_REGISTRY_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_REGISTRY_BYTES,
        "{label} exceeds 1 MiB"
    );
    Ok(Some(
        serde_json::from_slice(&bytes).with_context(|| format!("invalid {label} JSON"))?,
    ))
}

fn payload(path: &Path, label: &str) -> Result<Value> {
    let value = read_json(path, label)?.unwrap_or_else(|| json!({"schema_version":1,"agents":{}}));
    ensure!(
        value["schema_version"].as_u64() == Some(1),
        "unsupported {label} schema"
    );
    ensure!(
        value["agents"].is_object(),
        "{label} agents must be an object"
    );
    Ok(value)
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut data = vec![0_u8; bytes];
    File::open("/dev/urandom")
        .context("opening operating-system random source")?
        .read_exact(&mut data)
        .context("reading operating-system random source")?;
    Ok(data.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let suffix = random_hex(8)?;
    let temp = path.with_extension(format!("{}.{}.tmp", std::process::id(), suffix));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("creating {}", temp.display()))?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        File::open(path.parent().context("registry path has no parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn parse_records(value: &Value, path: &Path) -> Result<BTreeMap<String, Record>> {
    let agents = value["agents"]
        .as_object()
        .context("registry agents must be an object")?;
    let mut records = BTreeMap::new();
    for (name, entry) in agents {
        validate_identity(name)?;
        ensure!(entry.is_object(), "agent {name:?} entry must be an object");
        let port = entry["port"]
            .as_u64()
            .filter(|p| (1..=65_535).contains(p))
            .with_context(|| format!("agent {name:?} has no usable port"))?
            as u16;
        let token = entry["token"]
            .as_str()
            .filter(|s| s.len() >= 16 && !s.chars().any(char::is_control))
            .with_context(|| format!("agent {name:?} has no usable token"))?
            .to_owned();
        let generation = match entry["generation"].as_str() {
            Some(generation) if !generation.is_empty() => generation.to_owned(),
            None if entry["generation"].is_null() => {
                let mut hash = Sha256::new();
                hash.update(format!("legacy\0{name}\0{port}\0{token}"));
                format!("{:x}", hash.finalize())[..32].to_owned()
            }
            _ => bail!("agent {name:?} has no usable generation"),
        };
        records.insert(
            name.clone(),
            Record {
                name: name.clone(),
                port,
                token,
                generation,
            },
        );
    }
    ensure!(
        !records.is_empty(),
        "registry {} lists no agents",
        path.display()
    );
    Ok(records)
}

pub fn load(state_dir: &Path) -> Result<BTreeMap<String, Record>> {
    let path = state_dir.join("agents.json");
    ensure!(
        path.is_file(),
        "registry {} missing; run the init command first",
        path.display()
    );
    parse_records(&payload(&path, "registry")?, &path)
}

fn port_for(agents: &Map<String, Value>, name: &str, requested: Option<u16>) -> Result<u16> {
    validate_identity(name)?;
    let existing = agents.get(name);
    let port =
        if let Some(entry) = existing {
            ensure!(entry.is_object(), "agent {name:?} entry must be an object");
            let previous = entry["port"]
                .as_u64()
                .filter(|p| (1..=65_535).contains(p))
                .with_context(|| format!("agent {name:?} has no usable port"))?
                as u16;
            ensure!(requested.is_none_or(|port| port == previous),
            "agent {name:?} already uses port {previous}; refusing requested port {requested:?}");
            previous
        } else {
            requested
                .or_else(|| {
                    KNOWN_PORTS
                        .iter()
                        .find(|(identity, _)| *identity == name)
                        .map(|(_, port)| *port)
                })
                .with_context(|| format!("unknown agent {name:?} requires --port"))?
        };
    ensure!(
        port != 0,
        "invalid agent port 0; expected integer in 1..65535"
    );
    for (other, entry) in agents {
        if other != name && entry["port"].as_u64() == Some(u64::from(port)) {
            bail!("port {port} is already assigned to agent {other:?}");
        }
    }
    Ok(port)
}

pub fn serve_port(state_dir: &Path, name: &str, requested: Option<u16>) -> Result<u16> {
    let _lock = RegistryLock::acquire(state_dir)?;
    let value = payload(&state_dir.join("agents.json"), "registry")?;
    port_for(
        value["agents"].as_object().context("invalid agents")?,
        name,
        requested,
    )
}

pub fn init(
    state_dir: &Path,
    name: Option<&str>,
    requested: Option<u16>,
    renew: bool,
) -> Result<BTreeMap<String, Record>> {
    let _lock = RegistryLock::acquire(state_dir)?;
    let path = state_dir.join("agents.json");
    let mut value = payload(&path, "registry")?;
    ensure!(
        name.is_some() || requested.is_none(),
        "--port requires --name"
    );
    if let Some(name) = name {
        let agents = value["agents"].as_object_mut().context("invalid agents")?;
        let port = port_for(agents, name, requested)?;
        let previous = agents.get(name);
        let token = previous
            .and_then(|record| record["token"].as_str())
            .filter(|token| token.len() >= 16)
            .map(str::to_owned)
            .map(Ok)
            .unwrap_or_else(|| random_hex(24))?;
        let generation = if renew {
            None
        } else {
            previous
                .and_then(|record| record["generation"].as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let generation = generation.map(Ok).unwrap_or_else(|| random_hex(16))?;
        agents.insert(
            name.to_owned(),
            json!({"port":port,"token":token,"generation":generation}),
        );
    }
    atomic_json(&path, &value)?;
    if value["agents"].as_object().is_some_and(Map::is_empty) {
        return Ok(BTreeMap::new());
    }
    parse_records(&value, &path)
}

fn probe(record: &Record) -> Value {
    let base = json!({"name":record.name,"port":record.port,
        "registration_fingerprint":record.fingerprint()});
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_redirects(0)
        .proxy(None)
        .max_response_header_size(16_384)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let card = (|| -> Result<Value> {
        let mut response = agent
            .get(format!("{}/.well-known/agent-card.json", record.base_url()))
            .call()
            .context("card probe failed")?;
        ensure!(
            response.status().as_u16() == 200,
            "card fetch returned HTTP {}",
            response.status()
        );
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_CARD_BYTES)
            .read_to_vec()?;
        let card: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            card["name"].as_str() == Some(&record.name),
            "card name does not match registry"
        );
        Ok(card)
    })();
    let mut result = base.as_object().expect("probe base object").clone();
    match card {
        Ok(card) => {
            result.insert("status".into(), json!("up"));
            result.insert(
                "card_version".into(),
                card.get("version").cloned().unwrap_or(Value::Null),
            );
            result.insert(
                "skills".into(),
                json!(card["skills"]
                    .as_array()
                    .map(|skills| skills
                        .iter()
                        .map(|skill| skill["id"].clone())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()),
            );
        }
        Err(error) => {
            result.insert("status".into(), json!("down"));
            result.insert(
                "reason".into(),
                json!(error.to_string().chars().take(200).collect::<String>()),
            );
        }
    }
    Value::Object(result)
}

pub fn peers(state_dir: &Path) -> Result<Vec<Value>> {
    let records = load(state_dir)?;
    let observations: Vec<Value> = records.values().map(probe).collect();
    record_probes(state_dir, &observations)
}

fn record_probes(state_dir: &Path, observations: &[Value]) -> Result<Vec<Value>> {
    let _lock = RegistryLock::acquire(state_dir)?;
    let records = load(state_dir)?;
    let path = state_dir.join("liveness.json");
    let mut value = payload(&path, "liveness")?;
    let states = value["agents"]
        .as_object_mut()
        .context("invalid liveness agents")?;
    let mut updated = Vec::new();
    let now = crate::instant::isoformat_millis_utc(crate::instant::now());
    for observation in observations {
        let name = observation["name"].as_str().context("probe has no name")?;
        let mut item = observation
            .as_object()
            .context("probe is not an object")?
            .clone();
        let current = records.get(name);
        if current
            .is_none_or(|record| observation["registration_fingerprint"] != record.fingerprint())
        {
            item.insert("status".into(), json!("stale"));
            item.insert(
                "reason".into(),
                json!("registration changed while probe was in flight"),
            );
            item.insert("consecutive_failures".into(), json!(0));
        } else {
            let prior = states
                .get(name)
                .and_then(|state| state["consecutive_failures"].as_u64())
                .unwrap_or(0);
            let failures = if observation["status"] == "up" {
                0
            } else {
                prior.saturating_add(1)
            };
            states.insert(name.to_owned(), json!({"consecutive_failures":failures,
                "last_probe_at":now,"registration_fingerprint":observation["registration_fingerprint"]}));
            item.insert("consecutive_failures".into(), json!(failures));
        }
        updated.push(Value::Object(item));
    }
    states.retain(|name, _| records.contains_key(name));
    atomic_json(&path, &value)?;
    Ok(updated)
}

pub fn reap(state_dir: &Path, threshold: u64) -> Result<Value> {
    ensure!(threshold > 0, "consecutive failures must be positive");
    let observations = peers(state_dir)?;
    let candidates: BTreeMap<String, String> = observations
        .iter()
        .filter(|item| {
            item["status"] == "down"
                && item["consecutive_failures"].as_u64().unwrap_or(0) >= threshold
        })
        .filter_map(|item| {
            Some((
                item["name"].as_str()?.to_owned(),
                item["registration_fingerprint"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let _lock = RegistryLock::acquire(state_dir)?;
    let registry_path = state_dir.join("agents.json");
    let liveness_path = state_dir.join("liveness.json");
    let mut registry = payload(&registry_path, "registry")?;
    let records = parse_records(&registry, &registry_path)?;
    let mut liveness = payload(&liveness_path, "liveness")?;
    let mut removed = Vec::new();
    for (name, fingerprint) in candidates {
        let count = liveness["agents"][&name]["consecutive_failures"]
            .as_u64()
            .unwrap_or(0);
        if records
            .get(&name)
            .is_some_and(|record| record.fingerprint() == fingerprint)
            && liveness["agents"][&name]["registration_fingerprint"] == fingerprint
            && count >= threshold
        {
            registry["agents"]
                .as_object_mut()
                .context("invalid registry agents")?
                .remove(&name);
            liveness["agents"]
                .as_object_mut()
                .context("invalid liveness agents")?
                .remove(&name);
            removed.push(name);
        }
    }
    if !removed.is_empty() {
        atomic_json(&registry_path, &registry)?;
        atomic_json(&liveness_path, &liveness)?;
    }
    Ok(json!({"consecutive_failures":threshold,"probed":observations,"reaped":removed}))
}

pub fn summary(state_dir: &Path, records: &BTreeMap<String, Record>) -> Value {
    let agents: Map<String, Value> = records
        .iter()
        .map(|(name, record)| (name.clone(), json!({"port":record.port})))
        .collect();
    json!({"registry":state_dir.join("agents.json").display().to_string(),"agents":agents})
}
