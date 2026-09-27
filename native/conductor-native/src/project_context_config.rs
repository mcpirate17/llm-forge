//! Strict TOML schema parsing without Python or filesystem access.

use serde_json::{json, Value};

type ConfigPaths = (Option<String>, Option<String>, Vec<String>);

#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub code: &'static str,
    pub message: String,
}

impl ConfigError {
    fn parse() -> Self {
        Self {
            code: "CONFIG_PARSE",
            message: "configuration is not strict UTF-8 TOML".into(),
        }
    }

    fn schema(message: impl Into<String>) -> Self {
        Self {
            code: "CONFIG_SCHEMA",
            message: message.into(),
        }
    }
}

fn parse_paths(root: &toml::map::Map<String, toml::Value>) -> Result<ConfigPaths, ConfigError> {
    let paths = match root.get("paths") {
        None => toml::map::Map::new(),
        Some(value) => value
            .as_table()
            .cloned()
            .ok_or_else(|| ConfigError::schema("[paths] has unknown keys"))?,
    };
    if paths
        .keys()
        .any(|key| !matches!(key.as_str(), "policy" | "registry" | "notes"))
    {
        return Err(ConfigError::schema("[paths] has unknown keys"));
    }
    let path_value = |field: &str| -> Result<Option<String>, ConfigError> {
        match paths.get(field) {
            None => Ok(None),
            Some(value) => value
                .as_str()
                .filter(|item| !item.is_empty() && !item.contains(['\0', '\r', '\n']))
                .map(|item| Some(item.to_owned()))
                .ok_or_else(|| {
                    ConfigError::schema(format!("paths.{field} must be a non-empty string"))
                }),
        }
    };
    let notes = match paths.get("notes") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                ConfigError::schema("paths.notes must be an array of non-empty strings")
            })?
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|text| !text.is_empty() && !text.contains(['\0', '\r', '\n']))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        ConfigError::schema("paths.notes must be an array of non-empty strings")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok((path_value("policy")?, path_value("registry")?, notes))
}

pub fn parse_config(raw: &[u8]) -> Result<Value, ConfigError> {
    let text = std::str::from_utf8(raw).map_err(|_| ConfigError::parse())?;
    let parsed = text
        .parse::<toml::Value>()
        .map_err(|_| ConfigError::parse())?;
    let root = parsed
        .as_table()
        .ok_or_else(|| ConfigError::schema("configuration must be a table"))?;
    if root
        .keys()
        .any(|key| !matches!(key.as_str(), "schema_version" | "project" | "paths"))
    {
        return Err(ConfigError::schema(
            "configuration contains unknown top-level keys",
        ));
    }
    if root.get("schema_version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(ConfigError::schema("schema_version must be integer 1"));
    }
    let project = match root.get("project") {
        None => toml::map::Map::new(),
        Some(value) => value
            .as_table()
            .cloned()
            .ok_or_else(|| ConfigError::schema("[project] only supports id"))?,
    };
    if project.keys().any(|key| key != "id") {
        return Err(ConfigError::schema("[project] only supports id"));
    }
    let project_id = match project.get("id") {
        None => None,
        Some(value) => {
            let value = value.as_str().ok_or_else(|| {
                ConfigError::schema(
                    "project.id must be a trimmed 1-128 character non-control string",
                )
            })?;
            if value.is_empty()
                || value.chars().count() > 128
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err(ConfigError::schema(
                    "project.id must be a trimmed 1-128 character non-control string",
                ));
            }
            Some(value)
        }
    };
    let (policy, registry, notes) = parse_paths(root)?;
    Ok(json!({"project_id":project_id, "policy":policy,
        "registry":registry, "notes":notes}))
}

#[cfg(test)]
mod tests {
    use super::parse_config;
    use serde_json::json;

    #[test]
    fn valid_config_yields_normalized_fields() {
        let parsed =
            parse_config(b"schema_version=1\n[project]\nid='sample'\n[paths]\nnotes=['notes']\n")
                .unwrap();
        assert_eq!(
            parsed,
            json!({"project_id":"sample", "policy":null,
            "registry":null, "notes":["notes"]})
        );
    }

    #[test]
    fn schema_and_parse_errors_stay_distinct() {
        assert_eq!(parse_config(b"\xff").unwrap_err().code, "CONFIG_PARSE");
        assert_eq!(
            parse_config(b"schema_version=2\n").unwrap_err().code,
            "CONFIG_SCHEMA"
        );
        assert_eq!(
            parse_config(b"schema_version=1\n[paths]\nnotes=['']\n")
                .unwrap_err()
                .message,
            "paths.notes must be an array of non-empty strings"
        );
    }
}
