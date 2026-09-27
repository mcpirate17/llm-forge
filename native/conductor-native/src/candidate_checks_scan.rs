use super::{array, string, Finding};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

static PRIVATE_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").unwrap());
static AWS_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap());
static GITHUB_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgh[psoru]_[A-Za-z0-9_]{30,}\b").unwrap());
static GENERIC_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:api[_-]?key|secret|token|password)\s*[:=]\s*['"][A-Za-z0-9_./+=-]{20,}['"]"#,
    )
    .unwrap()
});
static UNSAFE_NATIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:gets|strcpy|strcat|sprintf|system|popen)\s*\(").unwrap());

fn line(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

pub fn secret_scan(payload: &Value) -> Result<Value, String> {
    let mut findings = Vec::new();
    for file in array(payload, "files")? {
        let path = string(file, "path")?;
        let text = string(file, "text")?;
        for (rule, pattern) in [
            ("private-key", &*PRIVATE_KEY),
            ("aws-access-key", &*AWS_KEY),
            ("github-token", &*GITHUB_TOKEN),
            ("generic-api-key", &*GENERIC_KEY),
        ] {
            for matched in pattern.find_iter(text) {
                findings.push(
                    Finding::new(
                        "secret-scan",
                        rule,
                        "critical",
                        "candidate contains secret-like credential material",
                    )
                    .path(path)
                    .line(line(text, matched.start()))
                    .help("Remove and rotate the credential; do not baseline live secrets."),
                );
            }
        }
    }
    Ok(json!({"findings": findings}))
}

pub fn native_source(payload: &Value) -> Result<Value, String> {
    let mut findings = Vec::new();
    for file in array(payload, "files")? {
        let path = string(file, "path")?;
        let text = string(file, "text")?;
        for matched in UNSAFE_NATIVE.find_iter(text) {
            findings.push(
                Finding::new(
                    "native-source",
                    "unsafe-native-api",
                    "critical",
                    format!("unsafe native API admitted: {}", matched.as_str().trim()),
                )
                .path(path)
                .line(line(text, matched.start())),
            );
        }
    }
    Ok(json!({"findings": findings}))
}
