//! Provider-hook config merge rules for the portable A2A session-start installer.
//!
//! The Python boundary owns the filesystem, atomic writes and the CLI. Rust owns
//! the pure decision core: POSIX-ish shlex tokenizing/quoting, managed-hook
//! detection, the hook-tree transform, and the install merge.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::{Map, Value};

fn shlex_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Python ``shlex.split(command, posix=True)``: quotes abut, backslash escapes
/// outside quotes, inside double quotes it escapes only ``"`` and ``\``.
fn shlex_split(command: &str) -> Result<Vec<String>, String> {
    let mut parts: Vec<String> = Vec::new();
    let mut token = String::new();
    let mut have_token = false;
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if q == '"' && ch == '\\' {
                match chars.peek() {
                    Some(next) if matches!(*next, '"' | '\\') => {
                        token.push(chars.next().unwrap());
                    }
                    _ => token.push(ch),
                }
            } else {
                token.push(ch);
            }
            have_token = true;
        } else if shlex_whitespace(ch) {
            if have_token {
                parts.push(std::mem::take(&mut token));
                have_token = false;
            }
        } else if ch == '\'' {
            quote = Some('\'');
            have_token = true;
        } else if ch == '"' {
            quote = Some('"');
            have_token = true;
        } else if ch == '\\' {
            match chars.next() {
                Some(escaped) => token.push(escaped),
                None => return Err("No escaped character".to_owned()),
            }
            have_token = true;
        } else {
            token.push(ch);
            have_token = true;
        }
    }
    if quote.is_some() {
        return Err("No closing quotation".to_owned());
    }
    if have_token {
        parts.push(token);
    }
    Ok(parts)
}

/// Python ``shlex.join``: ``shlex.quote`` each part, space-join. A character
/// outside ``[a-zA-Z0-9_@%+=:,./-]`` forces single-quoting.
fn shlex_quote(part: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if part.is_empty() {
        return "''".to_owned();
    }
    if part.chars().all(safe) {
        return part.to_owned();
    }
    format!("'{}'", part.replace('\'', "'\"'\"'"))
}

fn shlex_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| shlex_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when ``parts`` contains the adjacent pair ``["-m", managed_module]``.
fn is_managed_command(parts: &[String], managed_module: &str) -> bool {
    if parts.len() < 2 {
        return false;
    }
    (0..parts.len() - 1).any(|index| parts[index] == "-m" && parts[index + 1] == managed_module)
}

fn is_managed_hook(value: &Value, managed_module: &str) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(command) = object.get("command").and_then(Value::as_str) else {
        return false;
    };
    match shlex_split(command) {
        Ok(parts) => is_managed_command(&parts, managed_module),
        Err(_) => false,
    }
}

/// Mirror ``hook_installer._without_managed_hooks``.
fn without_managed_hooks(config: &mut Value, managed_module: &str) -> Result<(), String> {
    let root_is_object = config.is_object();
    if !root_is_object {
        return Err("provider config root must be a JSON object".to_owned());
    }
    let Some(hooks) = config.get_mut("hooks") else {
        return Ok(());
    };
    if !hooks.is_object() {
        return Err("top-level 'hooks' must be a JSON object".to_owned());
    }
    let hooks_object = hooks.as_object_mut().expect("checked object");
    let events: Vec<String> = hooks_object.keys().cloned().collect();
    for event in events {
        let groups = hooks_object.get_mut(&event).expect("key from keys()");
        let Some(groups_list) = groups.as_array_mut() else {
            return Err(format!("hooks.{event} must be a JSON array"));
        };
        let mut kept_groups: Vec<Value> = Vec::new();
        for group in groups_list.iter() {
            let Some(group_object) = group.as_object() else {
                kept_groups.push(group.clone());
                continue;
            };
            let Some(commands) = group_object.get("hooks").and_then(Value::as_array) else {
                kept_groups.push(group.clone());
                continue;
            };
            let kept_commands: Vec<Value> = commands
                .iter()
                .filter(|command| !is_managed_hook(command, managed_module))
                .cloned()
                .collect();
            if !kept_commands.is_empty() {
                let mut new_group = group.clone();
                if let Some(slot) = new_group
                    .as_object_mut()
                    .and_then(|object| object.get_mut("hooks"))
                {
                    *slot = Value::Array(kept_commands);
                }
                kept_groups.push(new_group);
            } else if group_object
                .keys()
                .any(|key| key != "hooks" && key != "matcher")
            {
                let mut new_group = group.clone();
                if let Some(slot) = new_group
                    .as_object_mut()
                    .and_then(|object| object.get_mut("hooks"))
                {
                    *slot = Value::Array(Vec::new());
                }
                kept_groups.push(new_group);
            }
        }
        if kept_groups.is_empty() {
            hooks_object.remove(&event);
        } else {
            hooks_object.insert(event, Value::Array(kept_groups));
        }
    }
    if hooks_object.is_empty() {
        if let Some(root) = config.as_object_mut() {
            root.remove("hooks");
        }
    }
    Ok(())
}

fn parse_payload(payload: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(payload).map_err(|error| format!("invalid {what}: {error}"))
}

#[pyfunction]
#[pyo3(signature = (command))]
fn hook_installer_shlex_split_native(command: &str) -> PyResult<Vec<String>> {
    shlex_split(command).map_err(PyValueError::new_err)
}

#[pyfunction]
#[pyo3(signature = (parts))]
fn hook_installer_shlex_join_native(parts: Vec<String>) -> String {
    shlex_join(&parts)
}

#[pyfunction]
#[pyo3(signature = (command, managed_module))]
fn hook_installer_is_managed_native(command: &str, managed_module: &str) -> bool {
    match shlex_split(command) {
        Ok(parts) => is_managed_command(&parts, managed_module),
        Err(_) => false,
    }
}

#[pyfunction]
#[pyo3(signature = (config_json, managed_module))]
fn hook_installer_without_managed_native(
    config_json: &str,
    managed_module: &str,
) -> PyResult<String> {
    let mut config =
        parse_payload(config_json, "provider config").map_err(PyValueError::new_err)?;
    without_managed_hooks(&mut config, managed_module).map_err(PyValueError::new_err)?;
    Ok(config.to_string())
}

/// Mirror ``hook_installer.merge_install``: exactly one managed hook per spec.
#[pyfunction]
#[pyo3(signature = (config_json, spec_json, command, managed_name, managed_module))]
fn hook_installer_merge_install_native(
    config_json: &str,
    spec_json: &str,
    command: &str,
    managed_name: &str,
    managed_module: &str,
) -> PyResult<String> {
    let mut config =
        parse_payload(config_json, "provider config").map_err(PyValueError::new_err)?;
    let spec = parse_payload(spec_json, "provider spec").map_err(PyValueError::new_err)?;
    let spec_object = spec
        .as_object()
        .ok_or_else(|| PyValueError::new_err("provider spec must be a JSON object"))?;
    let event = spec_object
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| PyValueError::new_err("provider spec is missing \"event\""))?;
    let timeout = spec_object
        .get("timeout")
        .ok_or_else(|| PyValueError::new_err("provider spec is missing \"timeout\""))?;
    let include_matcher = spec_object
        .get("include_matcher")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let install_name = spec_object
        .get("install_hook_name")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    without_managed_hooks(&mut config, managed_module).map_err(PyValueError::new_err)?;
    let root = config.as_object_mut().expect("checked object above");
    let hooks = root
        .entry("hooks".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_object = hooks
        .as_object_mut()
        .expect("without_managed_hooks refused a non-object 'hooks'");

    let mut hook = Map::new();
    hook.insert("type".to_owned(), Value::String("command".to_owned()));
    hook.insert("command".to_owned(), Value::String(command.to_owned()));
    hook.insert("timeout".to_owned(), timeout.clone());
    if install_name {
        hook.insert("name".to_owned(), Value::String(managed_name.to_owned()));
    }
    let mut group = Map::new();
    group.insert("hooks".to_owned(), Value::Array(vec![Value::Object(hook)]));
    if include_matcher {
        group.insert("matcher".to_owned(), Value::String(String::new()));
    }

    let groups = hooks_object
        .entry(event.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups_list) = groups.as_array_mut() else {
        return Err(PyValueError::new_err(format!(
            "hooks.{event} must be a JSON array"
        )));
    };
    groups_list.push(Value::Object(group));
    Ok(config.to_string())
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(hook_installer_shlex_split_native, module)?)?;
    module.add_function(wrap_pyfunction!(hook_installer_shlex_join_native, module)?)?;
    module.add_function(wrap_pyfunction!(hook_installer_is_managed_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        hook_installer_without_managed_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        hook_installer_merge_install_native,
        module
    )?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_matches_posix_quoting_rules() {
        assert_eq!(
            shlex_split("python -m conductor.a2a_session_start --provider codex").unwrap(),
            vec![
                "python",
                "-m",
                "conductor.a2a_session_start",
                "--provider",
                "codex"
            ]
        );
        assert_eq!(shlex_split("'a b' c").unwrap(), vec!["a b", "c"]);
        assert_eq!(shlex_split("\"a b\"c").unwrap(), vec!["a bc"]);
        assert_eq!(shlex_split(r"a\ b").unwrap(), vec!["a b"]);
        assert_eq!(shlex_split(r#""a\"b""#).unwrap(), vec![r#"a"b"#]);
        assert_eq!(shlex_split("''").unwrap(), vec![""]);
    }

    #[test]
    fn split_refuses_unterminated_quotes_and_dangling_escapes() {
        assert_eq!(shlex_split("'open"), Err("No closing quotation".to_owned()));
        assert_eq!(shlex_split("\\"), Err("No escaped character".to_owned()));
    }

    #[test]
    fn quote_round_trips_through_split() {
        for part in [
            "plain",
            "with space",
            "it's",
            "",
            "$HOME",
            "a/b:c",
            "tricky'quote",
        ] {
            let joined = shlex_join(&[part.to_owned()]);
            let parsed = shlex_split(&joined).unwrap();
            assert_eq!(
                parsed,
                vec![part.to_owned()],
                "round trip failed for {part:?} via {joined:?}"
            );
        }
    }

    #[test]
    fn managed_detection_needs_the_adjacent_pair() {
        let managed = "conductor.a2a_session_start";
        let parts = shlex_split("python -m conductor.a2a_session_start --x").unwrap();
        assert!(is_managed_command(&parts, managed));
        let reordered = shlex_split("python -m x --m conductor.a2a_session_start").unwrap();
        assert!(!is_managed_command(&reordered, managed));
        // The pair only has to be adjacent, not first: a hook that carries provider
        // flags ahead of the module is still ours.
        let trailing =
            shlex_split("python --provider codex -m conductor.a2a_session_start").unwrap();
        assert!(is_managed_command(&trailing, managed));
        // A second -m later in the argv still names us, so the scan cannot stop at
        // the first -m it finds.
        let second = shlex_split("python -m other.module -m conductor.a2a_session_start").unwrap();
        assert!(is_managed_command(&second, managed));
        // --module is a different flag; only -m binds the module argument.
        let long_flag = shlex_split("python --module conductor.a2a_session_start x").unwrap();
        assert!(!is_managed_command(&long_flag, managed));
        assert!(!is_managed_hook(
            &serde_json::json!({"command": "'"}),
            managed
        ));
        assert!(!is_managed_hook(
            &serde_json::json!({"command": 7}),
            managed
        ));
        assert!(!is_managed_hook(
            &serde_json::json!("not an object"),
            managed
        ));
    }

    #[test]
    fn without_managed_prunes_only_owned_commands() {
        let mut config = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "", "hooks": [
                        {"type": "command", "command": "python -m conductor.a2a_session_start"},
                        {"type": "command", "command": "echo keep"}
                    ]}
                ],
                "PostToolUse": [
                    {"matcher": "", "hooks": [
                        {"type": "command", "command": "python -m conductor.a2a_session_start"}
                    ]}
                ]
            },
            "model": "opus"
        });
        without_managed_hooks(&mut config, "conductor.a2a_session_start").unwrap();
        assert!(config.get("PostToolUse").is_none());
        assert!(config.get("model").is_some());
        let groups = config["hooks"]["SessionStart"].as_array().unwrap();
        let kept = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["command"], "echo keep");
    }

    #[test]
    fn without_managed_keeps_groups_with_other_fields() {
        let mut config = serde_json::json!({
            "hooks": {"SessionStart": [
                {"matcher": "", "custom": 1, "hooks": [
                    {"command": "python -m conductor.a2a_session_start"}]}
            ]}
        });
        without_managed_hooks(&mut config, "conductor.a2a_session_start").unwrap();
        let groups = config["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups[0]["hooks"].as_array().unwrap().len(), 0);
        assert_eq!(groups[0]["custom"], 1);
    }

    #[test]
    fn merge_install_appends_one_group_and_is_idempotent() {
        let spec = serde_json::json!({"event": "SessionStart", "timeout": 15,
                                      "include_matcher": true, "install_hook_name": false});
        let command = "python -m conductor.a2a_session_start";
        let mut config = serde_json::json!({"model": "opus"});
        let first = hook_installer_merge_install_native(
            &config.to_string(),
            &spec.to_string(),
            command,
            "a2a-session-start",
            "conductor.a2a_session_start",
        )
        .unwrap();
        let second = hook_installer_merge_install_native(
            &first,
            &spec.to_string(),
            command,
            "a2a-session-start",
            "conductor.a2a_session_start",
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&second).unwrap();
        let groups = parsed["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "reinstall must not duplicate the hook");
        assert_eq!(parsed["model"], "opus");
        let _ = &mut config;
    }

    #[test]
    fn merge_refuses_non_object_roots_and_bad_hooks() {
        let spec = serde_json::json!({"event": "SessionStart", "timeout": 15});
        assert!(
            hook_installer_merge_install_native("[]", &spec.to_string(), "x", "n", "m").is_err()
        );
        assert!(hook_installer_merge_install_native(
            "{\"hooks\": []}",
            &spec.to_string(),
            "x",
            "n",
            "m"
        )
        .is_err());
    }
}
