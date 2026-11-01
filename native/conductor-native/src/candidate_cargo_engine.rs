//! Offline RustSec matching against caller-authenticated pinned evidence bytes.
//! Bundle freshness, authenticity and immutable acquisition are caller obligations.
use rustsec::{database::Query, Advisory, Collection, Lockfile};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

const MAX_BUNDLE: usize = 32 * 1024 * 1024;
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
const MAX_LOCK: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_WORK: usize = 1_000_000;
type Registry = BTreeMap<(String, String), (String, bool)>;
const MAX_REQUIREMENTS: usize = 64;
const MAX_REQUIREMENT_BYTES: usize = 256;
const MAX_PARSE_WORK: usize = 1_000_000;
struct PreparedAdvisory {
    advisory: Advisory,
    ranges: Vec<rustsec::osv::OsvRange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_version: u32,
    captured_at: u64,
    advisory_commit: String,
    advisories: Vec<AdvisoryRow>,
    registry: Vec<RegistryRow>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvisoryRow {
    path: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRow {
    name: String,
    version: String,
    checksum: String,
    yanked: bool,
}

pub struct Auditor {
    captured_at: u64,
    advisory_commit: String,
    advisory_count: usize,
    advisories: BTreeMap<String, Vec<PreparedAdvisory>>,
    registry: Registry,
}
#[derive(Serialize)]
pub struct Observation {
    pub packages_total: usize,
    pub crates_io_packages: usize,
    pub other_sources: Vec<OtherSource>,
    pub matches: Vec<AdvisoryMatch>,
    pub yanked: Vec<Yanked>,
}
#[derive(Serialize)]
pub struct OtherSource {
    pub name: String,
    pub version: String,
    pub source: String,
}
#[derive(Serialize)]
pub struct AdvisoryMatch {
    pub advisory_id: String,
    pub package: String,
    pub version: String,
    pub title: String,
    pub informational: Option<String>,
}
#[derive(Serialize)]
pub struct Yanked {
    pub name: String,
    pub version: String,
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}
fn advisory_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 17
        && bytes.starts_with(b"RUSTSEC-")
        && bytes[8..12].iter().all(u8::is_ascii_digit)
        && bytes[12] == b'-'
        && bytes[13..17].iter().all(u8::is_ascii_digit)
        && id != "RUSTSEC-0000-0000"
}
impl Auditor {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BUNDLE {
            return Err("audit bundle exceeds 32 MiB".into());
        }
        let bundle: Bundle =
            serde_json::from_slice(bytes).map_err(|_| "invalid audit bundle JSON")?;
        if bundle.schema_version != 1
            || bundle.captured_at == 0
            || !(hex(&bundle.advisory_commit, 40) || hex(&bundle.advisory_commit, 64))
        {
            return Err("invalid audit bundle identity".into());
        }
        if bundle.advisories.is_empty()
            || bundle.advisories.len() > 10_000
            || bundle.registry.len() > MAX_ROWS
        {
            return Err("audit bundle row count exceeds bounds or advisories empty".into());
        }
        let count = bundle.advisories.len();
        Ok(Self {
            captured_at: bundle.captured_at,
            advisory_commit: bundle.advisory_commit,
            advisory_count: count,
            advisories: parse_advisories(bundle.advisories)?,
            registry: parse_registry(bundle.registry)?,
        })
    }
    pub fn captured_at(&self) -> u64 {
        self.captured_at
    }
    pub fn advisory_commit(&self) -> &str {
        &self.advisory_commit
    }
    pub fn advisory_count(&self) -> usize {
        self.advisory_count
    }
    pub fn registry_count(&self) -> usize {
        self.registry.len()
    }
    pub fn audit(&self, lockfile_text: &str) -> Result<Observation, String> {
        let mut remaining = MAX_WORK;
        self.audit_with_budget(lockfile_text, &mut remaining)
    }
    pub fn audit_with_budget(
        &self,
        lockfile_text: &str,
        remaining_work: &mut usize,
    ) -> Result<Observation, String> {
        if *remaining_work > MAX_WORK {
            return Err("audit shared work budget exceeds 1000000".into());
        }
        if lockfile_text.len() > MAX_LOCK {
            return Err("Cargo.lock exceeds 8 MiB".into());
        }
        let lock: Lockfile = lockfile_text.parse().map_err(|_| "invalid Cargo.lock")?;
        if lock.packages.len() > MAX_ROWS {
            return Err("Cargo.lock package count exceeds 100000".into());
        }
        let mut result = Observation {
            packages_total: lock.packages.len(),
            crates_io_packages: 0,
            other_sources: Vec::new(),
            matches: Vec::new(),
            yanked: Vec::new(),
        };
        let mut budget = OutputBudget::new(&result)?;
        for package in &lock.packages {
            spend(remaining_work, 1)?;
            let name = package.name.to_string();
            let version = package.version.to_string();
            if name.len() > 128 || version.len() > 128 {
                return Err("lockfile package identity exceeds bounds".into());
            }
            if package
                .source
                .as_ref()
                .is_some_and(|s| s.is_default_registry())
            {
                result.crates_io_packages += 1;
                let entry = self
                    .registry
                    .get(&(name.clone(), version.clone()))
                    .ok_or("missing crates.io registry evidence")?;
                let checksum = package
                    .checksum
                    .as_ref()
                    .ok_or("crates.io package checksum missing")?
                    .to_string();
                if checksum != entry.0 {
                    return Err("crates.io registry checksum mismatch".into());
                }
                if entry.1 {
                    let item = Yanked {
                        name: name.clone(),
                        version: version.clone(),
                    };
                    budget.add(&item)?;
                    result.yanked.push(item);
                }
                let advisories = self.advisories.get(&name).map(Vec::as_slice).unwrap_or(&[]);
                let cost = advisories
                    .iter()
                    .try_fold(0usize, |total, advisory| {
                        total.checked_add(1 + advisory.ranges.len() * 34)
                    })
                    .ok_or("advisory work overflow")?;
                spend(remaining_work, cost)?;
                add_matches(package, advisories, &mut result.matches, &mut budget)?;
            } else {
                let source = package
                    .source
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "workspace".into());
                if source.len() > 4096 {
                    return Err("package source exceeds 4096 bytes".into());
                }
                let item = OtherSource {
                    name,
                    version,
                    source,
                };
                budget.add(&item)?;
                result.other_sources.push(item);
            }
        }
        crate::candidate_cargo_audit::serialized_size(&result, MAX_OUTPUT)
            .map_err(|_| "audit output exceeds 32 MiB")?;
        Ok(result)
    }
}

fn preflight_ranges(text: &str, remaining: &mut usize) -> Result<(), String> {
    let parts =
        rustsec::advisory::Parts::parse(text).map_err(|_| "invalid advisory front matter")?;
    let value: toml::Value =
        toml::from_str(parts.front_matter).map_err(|_| "invalid advisory TOML")?;
    let versions = value
        .get("versions")
        .and_then(toml::Value::as_table)
        .ok_or("advisory versions missing")?;
    let mut count = 0usize;
    for key in ["patched", "unaffected"] {
        if let Some(list) = versions.get(key) {
            let list = list
                .as_array()
                .ok_or("advisory version ranges must be arrays")?;
            count = count
                .checked_add(list.len())
                .ok_or("range count overflow")?;
            if count > MAX_REQUIREMENTS {
                return Err("advisory exceeds 64 version requirements".into());
            }
            for item in list {
                let text = item.as_str().ok_or("version requirement must be text")?;
                if text.len() > MAX_REQUIREMENT_BYTES {
                    return Err("version requirement exceeds 256 bytes".into());
                }
                for digits in text
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                {
                    let number: u64 = digits
                        .parse()
                        .map_err(|_| "version requirement number exceeds bounds")?;
                    if number >= u64::MAX - 1024 {
                        return Err("version requirement number exceeds bounds".into());
                    }
                }
            }
        }
    }
    spend(remaining, count.saturating_mul(count).max(1))
}

fn parse_advisories(
    rows: Vec<AdvisoryRow>,
) -> Result<BTreeMap<String, Vec<PreparedAdvisory>>, String> {
    let mut result: BTreeMap<String, Vec<PreparedAdvisory>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut work = MAX_PARSE_WORK;
    for row in rows {
        if row.text.len() > 128 * 1024 || row.path.len() > 4096 || !row.text.starts_with("```toml")
        {
            return Err("advisory size exceeds limit".into());
        }
        preflight_ranges(&row.text, &mut work)?;
        let mut advisory = std::panic::catch_unwind(|| Advisory::from_str(&row.text))
            .map_err(|_| "RustSec advisory parser panicked")?
            .map_err(|_| "invalid RustSec advisory")?;
        let id = advisory.id().to_string();
        let name = advisory.metadata.package.to_string();
        if !advisory_id(&id)
            || row.path != format!("crates/{name}/{id}.md")
            || name.len() > 128
            || advisory
                .metadata
                .source
                .as_ref()
                .is_some_and(|source| source.to_string().len() > 4096)
            || advisory
                .metadata
                .collection
                .is_some_and(|c| c != Collection::Crates)
            || advisory.title().is_empty()
            || advisory
                .metadata
                .informational
                .as_ref()
                .is_some_and(|kind| kind.as_str().len() > 128)
        {
            return Err("advisory identity, path, collection or title invalid".into());
        }
        if !ids.insert(id) {
            return Err("duplicate advisory ID".into());
        }
        advisory.metadata.collection = Some(Collection::Crates);
        let ranges =
            std::panic::catch_unwind(|| rustsec::osv::ranges_for_advisory(&advisory.versions))
                .map_err(|_| "RustSec range conversion panicked")?;
        if ranges.len() > MAX_REQUIREMENTS + 1 {
            return Err("too many computed advisory ranges".into());
        }
        result
            .entry(name)
            .or_default()
            .push(PreparedAdvisory { advisory, ranges });
    }
    Ok(result)
}
fn parse_registry(rows: Vec<RegistryRow>) -> Result<Registry, String> {
    let mut result = BTreeMap::new();
    for row in rows {
        if row.name.len() > 128 || row.version.len() > 128 || !hex(&row.checksum, 64) {
            return Err("registry entry exceeds bounds or has invalid checksum".into());
        }
        row.name
            .parse::<rustsec::package::Name>()
            .map_err(|_| "invalid registry package name")?;
        let version = row
            .version
            .parse::<rustsec::Version>()
            .map_err(|_| "invalid registry version")?;
        if version.to_string() != row.version {
            return Err("registry version is not canonical".into());
        }
        if result
            .insert((row.name, row.version), (row.checksum, row.yanked))
            .is_some()
        {
            return Err("duplicate registry name and version".into());
        }
    }
    Ok(result)
}
fn add_matches(
    package: &rustsec::package::Package,
    advisories: &[PreparedAdvisory],
    out: &mut Vec<AdvisoryMatch>,
    budget: &mut OutputBudget,
) -> Result<(), String> {
    let mut query = Query::crate_scope().package_name(package.name.clone());
    if let Some(source) = &package.source {
        query = query.package_source(source.clone());
    }
    let info = query.clone().informational(true);
    for prepared in advisories {
        let advisory = &prepared.advisory;
        let scope = if advisory.metadata.informational.is_some() {
            &info
        } else {
            &query
        };
        if scope.matches(advisory)
            && prepared
                .ranges
                .iter()
                .any(|range| range.affects(&package.version))
        {
            let row = AdvisoryMatch {
                advisory_id: advisory.id().to_string(),
                package: package.name.to_string(),
                version: package.version.to_string(),
                title: bounded_title(advisory.title()),
                informational: advisory
                    .metadata
                    .informational
                    .as_ref()
                    .map(ToString::to_string),
            };
            budget.add(&row)?;
            out.push(row);
        }
    }
    Ok(())
}
fn spend(remaining: &mut usize, amount: usize) -> Result<(), String> {
    *remaining = remaining
        .checked_sub(amount)
        .ok_or("advisory match work exceeds shared budget")?;
    Ok(())
}
fn bounded_title(title: &str) -> String {
    if title.len() <= 4096 {
        return title.to_owned();
    }
    let mut end = 4093;
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &title[..end])
}
struct OutputBudget(usize);
impl OutputBudget {
    fn new(value: &Observation) -> Result<Self, String> {
        let size = crate::candidate_cargo_audit::serialized_size(value, MAX_OUTPUT)
            .map_err(|_| "audit output exceeds 32 MiB")?;
        Ok(Self(size))
    }
    fn add(&mut self, value: &impl Serialize) -> Result<(), String> {
        let size = crate::candidate_cargo_audit::serialized_size(value, MAX_OUTPUT)
            .map_err(|_| "audit output exceeds 32 MiB")?;
        self.0 = self
            .0
            .checked_add(size + 1)
            .ok_or("audit output size overflow")?;
        if self.0 > MAX_OUTPUT {
            return Err("audit output exceeds 32 MiB".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "candidate_cargo_engine_tests.rs"]
mod tests;
