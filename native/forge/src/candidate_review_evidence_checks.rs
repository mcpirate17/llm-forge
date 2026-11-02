use super::*;
use crate::candidate_source::lines::RangePlan;

pub(super) fn performance(
    source: &Source,
    candidate: &Candidate,
    policy: &Policy,
    inputs: &mut Inputs<'_>,
    ledger: &mut BTreeMap<String, Value>,
) -> Result<CheckResult> {
    let started = Instant::now();
    let changes: Vec<_> = candidate
        .changes
        .iter()
        .filter(|row| !row.deleted())
        .map(|row| json!({"path":row.path,"classes":row.classes}))
        .collect();
    let chosen = conductor_native::candidate_checks::evaluate(
        "performance-selection",
        &json!({"changes":changes,"hot_globs":policy.document["hot_path_globs"]}),
    )
    .map_err(anyhow::Error::msg)?;
    let hot: Vec<String> = serde_json::from_value(chosen["hot_paths"].clone())?;
    let evidence: Vec<String> = serde_json::from_value(chosen["evidence_paths"].clone())?;
    let mut rows = Vec::new();
    let mut receipts = Vec::new();
    if !hot.is_empty() {
        for path in &evidence {
            if path.ends_with(".json") {
                let bytes = read(inputs, path, 2 * 1024 * 1024, ledger)?;
                let receipt = std::str::from_utf8(&bytes)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| {
                        crate::tool_schema::parse_document_with_limit(text, 2 * 1024 * 1024)
                    });
                match receipt {
                    Ok(receipt) if receipt["schema"] == "forge.performance-evidence.v1" => {
                        receipts.push(json!({"path":path,"receipt":receipt}))
                    }
                    Ok(_) => {}
                    Err(error) => {
                        receipts.push(json!({"path":path,"error":format!("{path}: {error}")}))
                    }
                }
            }
        }
        for change in candidate
            .changes
            .iter()
            .filter(|row| hot.contains(&row.path))
        {
            let bytes = read(inputs, &change.path, 1024 * 1024, ledger)?;
            let mut row = json!({"path":change.path,"classes":change.classes,
                "sha256":format!("{:x}",Sha256::digest(&bytes))});
            if change.classes.iter().any(|c| c == "python")
                && !change.classes.iter().any(|c| c == "native")
            {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    row["text"] = json!(text);
                }
            }
            rows.push(row);
        }
    }
    let files = if hot.is_empty() {
        Vec::new()
    } else {
        hot.into_iter().chain(evidence).collect()
    };
    evaluate(
        "performance-evidence",
        json!({"hot_changes":rows,"performance_receipts":receipts,
        "expected_host":source.root().to_str().context("source root must be UTF-8")?,
        "max_regression_percent":10.0}),
        files,
        json!({"execution_verified":false,"receipt_scope":"numerical self-consistency and current source hashes"}),
        started,
    )
}

pub(super) fn research(
    source: &Source,
    candidate: &Candidate,
    tests: &[String],
    inputs: &mut Inputs<'_>,
    ledger: &mut BTreeMap<String, Value>,
) -> Result<(CheckResult, RangePlan)> {
    let started = Instant::now();
    let novel: Vec<_> = candidate
        .changes
        .iter()
        .filter(|row| {
            !row.deleted()
                && row.classes.iter().any(|c| c == "novel")
                && !row.classes.iter().any(|c| c == "test")
        })
        .map(|row| row.path.clone())
        .collect();
    let ranges = source.changed_ranges(
        candidate,
        &novel.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let mut trigger = String::new();
    let mut full = Vec::new();
    let mut test_text = String::new();
    if !novel.is_empty() {
        for path in &novel {
            let bytes = read(inputs, path, 1024 * 1024, ledger)?;
            let text = String::from_utf8(bytes).context("novel evidence must be UTF-8")?;
            let file = ranges
                .files
                .iter()
                .find(|row| row.path == *path)
                .context("native changed-line plan omitted novel path")?;
            changed_text(&text, &file.ranges, &mut trigger)?;
            full.push(text);
        }
        for path in tests {
            let bytes = read(inputs, path, 1024 * 1024, ledger)?;
            test_text.push_str(
                std::str::from_utf8(&bytes).context("selected test evidence must be UTF-8")?,
            );
        }
    }
    let mut combined = full.join("\n");
    combined.push('\n');
    combined.push_str(&test_text);
    let folded = casefold(&combined)?;
    let files = if novel.is_empty() {
        Vec::new()
    } else {
        novel.iter().chain(tests).cloned().collect()
    };
    let result = evaluate(
        "research-integrity",
        json!({"changed_text":trigger,"combined_casefold":folded,
        "test_text":test_text,"changed_paths":novel}),
        files,
        json!({"armed_on":"changed-lines","diff_error":"","test_execution_verified":false,
            "selected_test_scope":"captured source text only; no execution proof"}),
        started,
    )?;
    Ok((result, ranges))
}

fn changed_text(
    text: &str,
    ranges: &[crate::candidate_source::lines::LineRange],
    output: &mut String,
) -> Result<()> {
    let mut cursor = 0;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let number = u32::try_from(index + 1)?;
        while cursor < ranges.len() && ranges[cursor].end < number {
            cursor += 1;
        }
        if cursor < ranges.len() && ranges[cursor].start <= number {
            output.push_str(line.trim_end_matches(['\r', '\n']));
            output.push('\n');
        }
    }
    ensure!(
        output.len() <= 32 * 1024 * 1024,
        "research trigger exceeds 32 MiB"
    );
    Ok(())
}

// Reuse the native Unicode table in bounded UTF-8 chunks; full folding is
// context independent. Never replace full folding with ASCII/lowercase rules.
fn casefold(text: &str) -> Result<String> {
    let mut folded = String::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let chunk = conductor_native::candidate_unicode::casefold(&text[start..end])?;
        ensure!(
            folded.len() + chunk.len() <= 32 * 1024 * 1024,
            "research folded evidence exceeds 32 MiB"
        );
        folded.push_str(&chunk);
        start = end;
    }
    Ok(folded)
}
