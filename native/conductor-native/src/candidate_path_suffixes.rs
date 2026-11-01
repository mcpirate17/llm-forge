//! Literal current-basename suffix admission shared by normalization and selection.
use serde_json::Value;

pub(crate) struct PathSuffixes {
    suffixes: Vec<String>,
}
impl PathSuffixes {
    pub(crate) fn new(value: Option<&Value>) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self {
                suffixes: Vec::new(),
            });
        };
        let values = value.as_array().ok_or("path_suffixes must be an array")?;
        if values.len() > 64 {
            return Err("path_suffixes exceeds 64 entries".into());
        }
        let mut suffixes = Vec::<String>::with_capacity(values.len());
        for value in values {
            let suffix = value
                .as_str()
                .ok_or("path_suffixes entries must be strings")?;
            let bytes = suffix.as_bytes();
            if !(2..=32).contains(&bytes.len())
                || bytes[0] != b'.'
                || !bytes[1..]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
            {
                return Err(
                    "path_suffixes entries require '.' plus 1..31 ASCII alnum/_/+/- characters"
                        .into(),
                );
            }
            let normalized = suffix.to_ascii_lowercase();
            if suffixes.contains(&normalized) {
                return Err("path_suffixes contains a case-insensitive duplicate".into());
            }
            suffixes.push(normalized);
        }
        Ok(Self { suffixes })
    }
    pub(crate) fn into_strings(self) -> Vec<String> {
        self.suffixes
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        if self.suffixes.is_empty() {
            return true;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let Some(dot) = name.rfind('.') else {
            return false;
        };
        // Match pathlib's final suffix: a leading dot alone does not create one.
        if dot == 0 || dot + 1 == name.len() {
            return false;
        }
        let suffix = &name[dot..];
        self.suffixes
            .iter()
            .any(|allowed| suffix.eq_ignore_ascii_case(allowed))
    }
}

#[cfg(test)]
#[path = "candidate_path_suffixes_tests.rs"]
mod tests;
