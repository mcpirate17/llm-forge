use super::{array, string, Finding};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    sync::LazyLock,
};

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

const MAX_FILES: usize = 100_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FINDINGS: usize = 100_000;
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const INPUT_EMPTY: usize = b"{\"files\":[]}".len();
const ROW_FIELDS: usize = b"{\"path\":,\"text\":}".len();
#[derive(Clone, Copy)]
struct Limits {
    files: usize,
    path: usize,
    file: usize,
    input: usize,
    findings: usize,
    output: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            files: MAX_FILES,
            path: MAX_PATH_BYTES,
            file: MAX_FILE_BYTES,
            input: MAX_INPUT_BYTES,
            findings: MAX_FINDINGS,
            output: MAX_OUTPUT_BYTES,
        }
    }
}
#[derive(Clone, Copy)]
enum Scan {
    Secret,
    Native,
}
struct Input<'a> {
    path: &'a str,
    text: &'a str,
}
struct Counter {
    used: usize,
    limit: usize,
}
impl Counter {
    fn add(&mut self, bytes: usize) -> io::Result<()> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|size| *size <= self.limit)
            .ok_or_else(|| io::Error::other("candidate scan byte budget exceeded"))?;
        Ok(())
    }
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.add(bytes.len())?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_input_fits(rows: &[Input<'_>], limit: usize) -> Result<(), String> {
    let mut counter = Counter {
        used: INPUT_EMPTY,
        limit,
    };
    for (index, file) in rows.iter().enumerate() {
        counter
            .add(ROW_FIELDS + usize::from(index != 0))
            .map_err(|_| "candidate scan input exceeds byte limit")?;
        serde_json::to_writer(&mut counter, file.path)
            .map_err(|_| "candidate scan input exceeds byte limit")?;
        serde_json::to_writer(&mut counter, file.text)
            .map_err(|_| "candidate scan input exceeds byte limit")?;
    }
    Ok(())
}
fn inputs(payload: &Value, limits: Limits) -> Result<Vec<Input<'_>>, String> {
    let files = array(payload, "files")?;
    if files.len() > limits.files {
        return Err("candidate scan file count exceeds limit".into());
    }
    let mut minimum = Counter {
        used: INPUT_EMPTY,
        limit: limits.input,
    };
    if minimum.used > minimum.limit {
        return Err("candidate scan input exceeds byte limit".into());
    }
    let mut maximum = Some(INPUT_EMPTY);
    let mut rows = Vec::with_capacity(files.len());
    for file in files {
        let path = string(file, "path")?;
        let text = string(file, "text")?;
        if path.len() > limits.path {
            return Err("candidate scan path exceeds byte limit".into());
        }
        if text.len() > limits.file {
            return Err("candidate scan file exceeds byte limit".into());
        }
        let fixed = ROW_FIELDS + 4 + usize::from(!rows.is_empty());
        let raw = path
            .len()
            .checked_add(text.len())
            .ok_or("scan size overflow")?;
        minimum
            .add(fixed)
            .map_err(|_| "candidate scan input exceeds byte limit")?;
        minimum
            .add(raw)
            .map_err(|_| "candidate scan input exceeds byte limit")?;
        // A UTF-8 input byte emits between one and six JSON bytes. Small
        // payloads can therefore be admitted without rescanning their text.
        maximum =
            maximum.and_then(|bytes| bytes.checked_add(fixed)?.checked_add(raw.checked_mul(6)?));
        rows.push(Input { path, text });
    }
    if !maximum.is_some_and(|bytes| bytes <= limits.input) {
        encoded_input_fits(&rows, limits.input)?;
    }
    Ok(rows)
}
struct Findings {
    rows: Vec<Finding>,
    bytes: Counter,
    maximum: usize,
}
impl Findings {
    fn new(limits: Limits) -> Result<Self, String> {
        let used = b"{\"findings\":[]}".len();
        if used > limits.output {
            return Err("candidate scan output exceeds byte limit".into());
        }
        Ok(Self {
            rows: Vec::new(),
            bytes: Counter {
                used,
                limit: limits.output,
            },
            maximum: limits.findings,
        })
    }
    fn push(&mut self, finding: Finding) -> Result<(), String> {
        if self.rows.len() >= self.maximum {
            return Err("candidate scan finding count exceeds limit".into());
        }
        if !self.rows.is_empty() {
            self.bytes
                .write_all(b",")
                .map_err(|_| "candidate scan output exceeds byte limit")?;
        }
        serde_json::to_writer(&mut self.bytes, &finding)
            .map_err(|_| "candidate scan output exceeds byte limit".to_owned())?;
        self.rows.push(finding);
        Ok(())
    }
}
fn newline_offsets(text: &str, offsets: &mut Vec<usize>) {
    offsets.clear();
    offsets.extend(
        text.bytes()
            .enumerate()
            .filter_map(|(i, byte)| (byte == b'\n').then_some(i)),
    );
}
fn line(offsets: &[usize], offset: usize) -> usize {
    offsets.partition_point(|newline| *newline < offset) + 1
}
struct Lines<'a> {
    text: &'a str,
    offsets: &'a mut Vec<usize>,
    built: bool,
}
impl Lines<'_> {
    fn at(&mut self, offset: usize) -> usize {
        // Clean files need no line index. Reuse its allocation only after the
        // first match, then preserve rule ordering through indexed lookup.
        if !self.built {
            newline_offsets(self.text, self.offsets);
            self.built = true;
        }
        line(self.offsets, offset)
    }
}
fn secrets(file: Input<'_>, lines: &mut Lines<'_>, findings: &mut Findings) -> Result<(), String> {
    for (rule, pattern) in [
        ("private-key", &*PRIVATE_KEY),
        ("aws-access-key", &*AWS_KEY),
        ("github-token", &*GITHUB_TOKEN),
        ("generic-api-key", &*GENERIC_KEY),
    ] {
        for matched in pattern.find_iter(file.text) {
            findings.push(
                Finding::new(
                    "secret-scan",
                    rule,
                    "critical",
                    "candidate contains secret-like credential material",
                )
                .path(file.path)
                .line(lines.at(matched.start()))
                .help("Remove and rotate the credential; do not baseline live secrets."),
            )?;
        }
    }
    Ok(())
}
fn native(file: Input<'_>, lines: &mut Lines<'_>, findings: &mut Findings) -> Result<(), String> {
    for matched in UNSAFE_NATIVE.find_iter(file.text) {
        findings.push(
            Finding::new(
                "native-source",
                "unsafe-native-api",
                "critical",
                format!("unsafe native API admitted: {}", matched.as_str().trim()),
            )
            .path(file.path)
            .line(lines.at(matched.start())),
        )?;
    }
    Ok(())
}
fn scan(payload: &Value, kind: Scan, limits: Limits) -> Result<Value, String> {
    let rows = inputs(payload, limits)?;
    let mut findings = Findings::new(limits)?;
    let mut offsets = Vec::new();
    for file in rows {
        let mut lines = Lines {
            text: file.text,
            offsets: &mut offsets,
            built: false,
        };
        match kind {
            Scan::Secret => secrets(file, &mut lines, &mut findings)?,
            Scan::Native => native(file, &mut lines, &mut findings)?,
        }
    }
    Ok(json!({"findings": findings.rows}))
}
pub fn secret_scan(payload: &Value) -> Result<Value, String> {
    scan(payload, Scan::Secret, Limits::default())
}
pub fn native_source(payload: &Value) -> Result<Value, String> {
    scan(payload, Scan::Native, Limits::default())
}

#[cfg(test)]
#[path = "candidate_checks_scan_tests.rs"]
mod tests;
