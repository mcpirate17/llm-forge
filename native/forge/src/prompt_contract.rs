//! Bounded lexical task-contract observations; no model, I/O or authority.
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_REQUEST_BYTES: usize = 524_288;
pub const MAX_TOTAL_TEXT_BYTES: usize = 65_536;
pub const MIN_OUTPUT_BYTES: usize = 512;
pub const MAX_CONSTRAINTS: usize = 32;
pub const MAX_OUTPUT_BYTES: usize = 262_144;
const MAX_EVIDENCE: usize = 256;
const LABELS: [&str; 5] = [
    "objective",
    "constraints",
    "acceptance",
    "verification",
    "performance",
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Inspect,
    Suggest,
    Plan,
    Compare,
}

fn present_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub mode: Mode,
    pub prompt: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default, deserialize_with = "present_string")]
    pub candidate: Option<String>,
    pub max_output_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Provenance {
    pub source: &'static str,
    pub sha256: String,
    pub bytes: usize,
}
#[derive(Debug, Serialize)]
pub struct Evidence {
    pub source: &'static str,
    pub kind: &'static str,
    pub label: &'static str,
    pub start: usize,
    pub end: usize,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
pub struct Advisory {
    pub label: &'static str,
    pub question: &'static str,
    pub caveat: &'static str,
}
#[derive(Debug, Serialize)]
pub struct ChecklistItem {
    pub evidence_index: usize,
    pub category: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Contract {
    pub prompt: String,
    pub constraints: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Retention {
    pub evidence_index: usize,
    pub literal_substring: bool,
}
#[derive(Debug, Serialize)]
pub struct Comparison {
    pub candidate: Provenance,
    pub prompt_is_literal_substring: bool,
    pub constraint_is_literal_substring: Vec<bool>,
    pub retention: Vec<Retention>,
    pub caveat: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Output {
    pub schema_version: u8,
    pub budget_scope: &'static str,
    pub caveat: &'static str,
    pub mode: Mode,
    pub prompt: Provenance,
    pub constraints: Vec<Provenance>,
    pub evidence: Vec<Evidence>,
    pub opaque: Vec<Evidence>,
    pub advisories: Vec<Advisory>,
    pub checklist: Vec<ChecklistItem>,
    pub execution_contract: Option<Contract>,
    pub comparison: Option<Comparison>,
}
fn provenance(source: &'static str, text: &str) -> Provenance {
    Provenance {
        source,
        sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        bytes: text.len(),
    }
}
fn validate(request: &Request) -> Result<(), String> {
    if request.prompt.trim().is_empty() {
        return Err("prompt must not be blank".into());
    }
    if request.constraints.len() > MAX_CONSTRAINTS {
        return Err("constraint count exceeds 32".into());
    }
    if request.constraints.iter().any(|s| s.trim().is_empty()) {
        return Err("constraints must not be blank".into());
    }
    if !(MIN_OUTPUT_BYTES..=MAX_OUTPUT_BYTES).contains(&request.max_output_bytes) {
        return Err("output budget must be 512..=262144 bytes".into());
    }
    if (request.mode == Mode::Compare) != request.candidate.is_some() {
        return Err("candidate is required only for compare".into());
    }
    if request
        .candidate
        .as_ref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return Err("candidate must not be blank".into());
    }
    let mut bytes = request.prompt.len();
    for text in request.constraints.iter().chain(request.candidate.iter()) {
        bytes = bytes.checked_add(text.len()).ok_or("input size overflow")?;
    }
    if bytes > MAX_TOTAL_TEXT_BYTES {
        return Err("prompt input exceeds 65536 bytes".into());
    }
    Ok(())
}
fn push(
    out: &mut Vec<Evidence>,
    kind: &'static str,
    label: &'static str,
    prompt: &str,
    start: usize,
    end: usize,
) -> Result<(), String> {
    if start == end || prompt[start..end].trim().is_empty() {
        return Ok(());
    }
    if out.len() >= MAX_EVIDENCE {
        return Err("evidence count exceeds 256".into());
    }
    out.push(Evidence {
        source: "prompt",
        kind,
        label,
        start,
        end,
        sha256: provenance("prompt", &prompt[start..end]).sha256,
    });
    Ok(())
}
fn category(label: &str) -> Option<&'static str> {
    match label.trim().to_ascii_lowercase().as_str() {
        "objective" | "goal" => Some("objective"),
        "constraint" | "constraints" => Some("constraints"),
        "acceptance" | "acceptance criteria" => Some("acceptance"),
        "verification" | "tests" | "test" => Some("verification"),
        "performance" => Some("performance"),
        _ => None,
    }
}
fn heading_name(text: &str) -> &str {
    let text = text.trim();
    let without_hashes = text.trim_end_matches('#');
    let text = if without_hashes.len() != text.len()
        && without_hashes
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
    {
        without_hashes.trim_end()
    } else {
        text
    };
    text.trim_end_matches(':').trim()
}
/// Returns (label, body offset); any Markdown heading closes the previous section.
fn header(line: &str) -> (bool, Option<(&'static str, usize)>) {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - trimmed.len();
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        let name = heading_name(&trimmed[hashes..]);
        return (true, category(name).map(|label| (label, line.len())));
    }
    if let Some(colon) = trimmed.find(':') {
        if let Some(label) = category(&trimmed[..colon]) {
            let body = &trimmed[colon + 1..];
            let leading = body.len() - body.trim_start_matches([' ', '\t']).len();
            return (true, Some((label, indent + colon + 1 + leading)));
        }
    }
    (false, None)
}
fn requirement(line: &str) -> bool {
    let mut text = line.trim_start_matches([' ', '\t']);
    if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
        text = rest;
    }
    let lowercase = text.to_ascii_lowercase();
    [
        "must ",
        "must not ",
        "do not ",
        "never ",
        "only ",
        "shall ",
        "required:",
    ]
    .iter()
    .any(|prefix| lowercase.starts_with(prefix))
}
fn close_section(
    active: &mut Option<(&'static str, usize)>,
    end: usize,
    prompt: &str,
    evidence: &mut Vec<Evidence>,
) -> Result<(), String> {
    if let Some((label, start)) = active.take() {
        push(evidence, "labeled_section", label, prompt, start, end)?;
    }
    Ok(())
}
#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
    start: usize,
}

fn opening_fence(text: &str, start: usize) -> Option<Fence> {
    if text.starts_with("<<<") {
        return Some(Fence {
            marker: b'>',
            length: 3,
            start,
        });
    }
    let marker = *text.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = text.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some(Fence {
        marker,
        length,
        start,
    })
}
fn closing_fence(text: &str, fence: Fence) -> bool {
    let length = text
        .bytes()
        .take_while(|byte| *byte == fence.marker)
        .count();
    length >= fence.length && text[length..].trim().is_empty()
}
/// Only leading one-line quoted text is recognized; backslashes escape a quote.
fn quoted_line(text: &str) -> Option<&'static str> {
    let quote = text.chars().next()?;
    if quote != '\"' && quote != '\'' {
        return None;
    }
    let mut escaped = false;
    for ch in text.chars().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some("quoted");
        }
    }
    Some("unterminated_quote")
}

struct Scan<'a> {
    prompt: &'a str,
    evidence: Vec<Evidence>,
    opaque: Vec<Evidence>,
    active: Option<(&'static str, usize)>,
    suspended: Option<&'static str>,
    fence: Option<Fence>,
}
impl Scan<'_> {
    fn pause(&mut self, offset: usize) -> Result<(), String> {
        self.suspended = self.active.map(|(label, _)| label);
        close_section(&mut self.active, offset, self.prompt, &mut self.evidence)
    }
    fn resume(&mut self, end: usize) {
        self.active = self.suspended.take().map(|label| (label, end));
    }
    fn line(&mut self, line: &str, offset: usize) -> Result<(), String> {
        let end = offset + line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start_matches([' ', '\t']);
        if let Some(fence) = self.fence {
            if closing_fence(trimmed, fence) {
                push(
                    &mut self.opaque,
                    "opaque_text",
                    "fenced",
                    self.prompt,
                    fence.start,
                    end,
                )?;
                self.fence = None;
                self.resume(end);
            }
        } else if let Some(fence) = opening_fence(trimmed, offset) {
            self.pause(offset)?;
            self.fence = Some(fence);
        } else if trimmed.starts_with('>') || quoted_line(trimmed).is_some() {
            let label = if trimmed.starts_with('>') {
                "quoted"
            } else {
                quoted_line(trimmed).unwrap_or("unterminated_quote")
            };
            self.pause(offset)?;
            push(
                &mut self.opaque,
                "opaque_text",
                label,
                self.prompt,
                offset,
                end,
            )?;
            self.resume(end);
        } else {
            let (is_header, recognized) = header(content);
            if is_header {
                close_section(&mut self.active, offset, self.prompt, &mut self.evidence)?;
                if let Some((label, body)) = recognized {
                    self.active = Some((
                        label,
                        if body == content.len() {
                            end
                        } else {
                            offset + body
                        },
                    ));
                }
            } else if requirement(content)
                && self.active.is_none_or(|(label, _)| label == "objective")
            {
                push(
                    &mut self.evidence,
                    "requirement_line",
                    "lexical_requirement",
                    self.prompt,
                    offset,
                    offset + content.len(),
                )?;
            }
        }
        Ok(())
    }
}
fn analyze_lines(prompt: &str) -> Result<(Vec<Evidence>, Vec<Evidence>), String> {
    let mut scan = Scan {
        prompt,
        evidence: Vec::new(),
        opaque: Vec::new(),
        active: None,
        suspended: None,
        fence: None,
    };
    let mut offset = 0;
    for line in prompt.split_inclusive('\n') {
        scan.line(line, offset)?;
        offset += line.len();
    }
    close_section(&mut scan.active, prompt.len(), prompt, &mut scan.evidence)?;
    if let Some(fence) = scan.fence {
        push(
            &mut scan.opaque,
            "opaque_text",
            "fenced",
            prompt,
            fence.start,
            prompt.len(),
        )?;
    }
    if scan.evidence.len() + scan.opaque.len() > MAX_EVIDENCE {
        return Err("evidence count exceeds 256".into());
    }
    scan.evidence
        .sort_by_key(|item| (item.start, item.end, item.kind));
    Ok((scan.evidence, scan.opaque))
}
fn advisories(mode: Mode, found: &[Evidence]) -> Vec<Advisory> {
    if !matches!(mode, Mode::Suggest | Mode::Plan) {
        return Vec::new();
    }
    let questions = [
        "Would an explicitly labeled objective help?",
        "Are there constraints to clarify?",
        "Are acceptance criteria needed?",
        "Would verification guidance help?",
        "Is a performance target relevant?",
    ];
    LABELS
        .iter()
        .zip(questions)
        .filter(|(label, _)| !found.iter().any(|row| row.label == **label))
        .map(|(label, question)| Advisory {
            label,
            question,
            caveat:
                "Not explicitly recognized; absence is not established. Optional question only.",
        })
        .collect()
}
/// Analyze supplied bytes only; recommendations and source content grant no authority.
pub fn analyze(request: &Request) -> Result<Output, String> {
    validate(request)?;
    let (evidence, opaque) = analyze_lines(&request.prompt)?;
    let suggestions = advisories(request.mode, &evidence);
    let checklist = if request.mode == Mode::Plan {
        evidence
            .iter()
            .enumerate()
            .map(|(evidence_index, item)| ChecklistItem {
                evidence_index,
                category: item.label,
            })
            .collect()
    } else {
        Vec::new()
    };
    let comparison = request.candidate.as_ref().map(|candidate| Comparison {
        candidate: provenance("candidate", candidate), prompt_is_literal_substring: candidate.contains(&request.prompt),
        constraint_is_literal_substring: request.constraints.iter().map(|s| candidate.contains(s)).collect(),
        retention: evidence.iter().enumerate().filter(|(_, item)| item.label != "objective").map(|(evidence_index, item)| Retention { evidence_index, literal_substring: candidate.contains(&request.prompt[item.start..item.end]) }).collect(),
        caveat: "Literal substring checks do not establish semantic equivalence or requirement preservation.",
    });
    let execution_contract = (request.mode == Mode::Plan).then(|| Contract {
        prompt: request.prompt.clone(),
        constraints: request.constraints.clone(),
    });
    let output = Output { schema_version: 1, budget_scope: "compact UTF-8 JSON payload; transport framing and model tokens excluded", caveat: "No authority hierarchy or semantic equivalence has been determined; lexical evidence may be ambiguous.", mode: request.mode,
        prompt: provenance("prompt", &request.prompt), constraints: request.constraints.iter().map(|s| provenance("caller_constraint", s)).collect(),
        evidence, opaque, advisories: suggestions, checklist, execution_contract, comparison };
    let bytes = serde_json::to_vec(&output)
        .map_err(|error| format!("cannot serialize task contract: {error}"))?
        .len();
    if bytes > request.max_output_bytes {
        return Err(format!(
            "protected output needs {bytes} bytes; budget is {}",
            request.max_output_bytes
        ));
    }
    Ok(output)
}
#[cfg(test)]
#[path = "prompt_contract_tests.rs"]
mod tests;
