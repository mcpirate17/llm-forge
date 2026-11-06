//! Verified generated workspace target selection; no caller-provided Cargo argv.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Target {
    #[serde(rename = "lib")]
    Lib {},
    #[serde(rename = "test")]
    Test { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub schema_version: u32,
    pub target: Target,
    pub filter: Option<String>,
}

fn identifier(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
impl Selector {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported workspace selector schema".into());
        }
        if let Target::Test { name } = &self.target {
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !name.as_bytes()[0].is_ascii_alphanumeric()
            {
                return Err("invalid workspace integration target name".into());
            }
        }
        if let Some(filter) = &self.filter {
            let trimmed = filter.strip_suffix("::").unwrap_or(filter);
            if filter.is_empty()
                || filter.len() > 256
                || trimmed.is_empty()
                || !trimmed.split("::").all(identifier)
            {
                return Err("invalid workspace libtest substring filter".into());
            }
        }
        Ok(())
    }
    pub fn cargo_args(&self) -> Vec<String> {
        vec![match &self.target {
            Target::Lib {} => "--lib".into(),
            Target::Test { name } => format!("--test={name}"),
        }]
    }
    pub fn libtest_args(&self) -> Vec<String> {
        self.filter
            .as_ref()
            .map(|f| vec!["--".into(), f.clone()])
            .unwrap_or_default()
    }
    pub fn engine_args(&self) -> Vec<String> {
        let mut args: Vec<String> = self
            .cargo_args()
            .iter()
            .map(|arg| format!("--cargo-arg={arg}"))
            .collect();
        args.extend(
            self.libtest_args()
                .iter()
                .map(|arg| format!("--cargo-test-arg={arg}")),
        );
        args
    }
}

/// Cargo metadata may describe a single library target using crate-type kinds.
pub fn is_library_kind(kind: &str) -> bool {
    matches!(
        kind,
        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
    )
}

pub fn is_workspace(profile: Option<&str>) -> bool {
    matches!(profile, Some("cargo-workspace-v2" | "cargo-workspace-v3"))
}

/// Decode only from a verified generated manifest's options.
pub fn parse(options: &Map<String, Value>) -> Result<Option<Selector>, String> {
    let profile = match options.get("execution_profile") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or("generator.options.execution_profile must be a string")?,
        ),
    };
    match profile {
        Some("cargo-workspace-v3") => {
            let value = options
                .get("workspace_selector")
                .ok_or("workspace v3 selector missing")?;
            let selector: Selector = serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid workspace selector: {e}"))?;
            selector.validate()?;
            Ok(Some(selector))
        }
        None | Some("cargo-workspace-v2") => {
            if options.contains_key("workspace_selector") {
                return Err("workspace selector requires v3 profile".into());
            }
            Ok(None)
        }
        Some(_) => Err("unsupported generator execution profile for campaign".into()),
    }
}

#[cfg(test)]
#[path = "mutation_workspace_selector_tests.rs"]
mod tests;
