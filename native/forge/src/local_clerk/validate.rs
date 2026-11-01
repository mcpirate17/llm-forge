use super::*;
use serde_json::Map;

const DOCUMENT_FIELDS: &[&str] = &[
    "schema_version",
    "authority",
    "task_class",
    "created_at",
    "model",
    "eval_count",
    "sources",
    "exact_duplicate_groups",
    "draft",
    "application",
];
const SOURCE_FIELDS: &[&str] = &[
    "path",
    "sha256",
    "size_bytes",
    "title",
    "truncated_for_prompt",
    "prompt_text_chars",
];
const DRAFT_FIELDS: &[&str] = &[
    "summary",
    "source_decisions",
    "open_tasks",
    "todo_items",
    "duplicate_candidates",
];

pub fn validate_document(settings: &Settings, path: &Path) -> Result<Value> {
    ensure!(
        path.file_name()
            .is_none_or(|name| name != ".current_work.md"),
        "direct .current_work.md ingestion is forbidden"
    );
    ensure!(
        path.as_os_str().len() <= MAX_PATH_BYTES,
        "draft path exceeds byte limit"
    );
    let bytes = crate::bounded_file::read(path, MAX_DOCUMENT_BYTES)?;
    let text = std::str::from_utf8(&bytes).context("clerk draft must be UTF-8")?;
    let value = crate::tool_schema::parse_document_with_limit(text, MAX_DOCUMENT_BYTES)?;
    validate_value(settings, &value)?;
    Ok(
        json!({"valid":true,"sources":value["sources"].as_array().unwrap().len(),
        "path":path.to_str().context("clerk draft path must be UTF-8")?}),
    )
}

/// Validate serialized provenance against current explicit source bytes.
/// The returned value is only structural/provenance validation, never approval.
pub fn validate_value(settings: &Settings, value: &Value) -> Result<()> {
    bounded_value(value, 0, &mut 0, &mut 0)?;
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_DOCUMENT_BYTES,
        "clerk document byte limit exceeded"
    );
    let document = exact_object(value, DOCUMENT_FIELDS, "document")?;
    ensure!(
        document["schema_version"].as_u64() == Some(1),
        "draft schema version is not supported"
    );
    for (key, expected) in [
        ("authority", "local-clerk-only"),
        ("task_class", "compaction"),
        ("model", MODEL),
        ("application", APPLICATION),
    ] {
        ensure!(
            document[key].as_str() == Some(expected),
            "draft {key} is invalid"
        );
    }
    let created = document["created_at"]
        .as_str()
        .context("draft created_at is invalid")?;
    ensure!(valid_timestamp(created), "draft created_at is invalid");
    ensure!(
        document["eval_count"].as_u64().is_some(),
        "draft eval_count is invalid"
    );
    let entries = document["sources"]
        .as_array()
        .context("draft has no sources")?;
    ensure!(
        !entries.is_empty() && entries.len() <= MAX_SOURCE_COUNT,
        "draft requires 1..3 unique sources"
    );
    let mut paths = Vec::with_capacity(entries.len());
    for entry in entries {
        let metadata = exact_object(entry, SOURCE_FIELDS, "source metadata")?;
        paths.push(PathBuf::from(
            metadata["path"]
                .as_str()
                .context("source path must be a string")?,
        ));
    }
    let sources = read_sources(settings, &paths)?;
    ensure!(
        sources.len() == entries.len(),
        "draft sources must be unique"
    );
    for (entry, source) in entries.iter().zip(&sources) {
        validate_source(entry, source)?;
    }
    ensure!(
        document["exact_duplicate_groups"] == json!(exact_duplicate_groups(&sources)),
        "draft duplicate groups do not match current sources"
    );
    normalize_model_draft(&document["draft"])?;
    Ok(())
}

fn exact_object<'a>(
    value: &'a Value,
    fields: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .with_context(|| format!("{label} must be an object"))?;
    ensure!(
        object.len() == fields.len() && fields.iter().all(|key| object.contains_key(*key)),
        "{label} has unexpected fields"
    );
    Ok(object)
}

fn validate_source(entry: &Value, source: &Source) -> Result<()> {
    ensure!(
        entry["sha256"].as_str() == Some(source.sha256.as_str()),
        "source changed since draft"
    );
    ensure!(
        entry["size_bytes"].as_u64() == Some(source.size_bytes)
            && entry["title"].as_str() == Some(source.title.as_str()),
        "source metadata changed since draft"
    );
    let chars = entry["prompt_text_chars"]
        .as_u64()
        .context("source prompt coverage is invalid")?;
    let length = source.text.chars().count() as u64;
    ensure!(
        chars >= 128.min(length) && chars <= length,
        "source prompt coverage is invalid"
    );
    ensure!(
        entry["truncated_for_prompt"].as_bool()
            == Some(
                source
                    .metadata(Some(usize::try_from(chars)?))
                    .truncated_for_prompt
            ),
        "source truncation metadata is invalid"
    );
    Ok(())
}

/// Keep legacy model normalization: bounded strings, ignore nonstring list items.
/// Validation deliberately checks the same shape rather than requiring pre-normalized lists.
pub fn normalize_model_draft(value: &Value) -> Result<Value> {
    let draft = exact_object(value, DRAFT_FIELDS, "local clerk draft")?;
    let summary = draft["summary"]
        .as_str()
        .context("local clerk draft lacks a summary string")?;
    let mut result = Map::new();
    result.insert(
        "summary".into(),
        json!(summary.chars().take(400).collect::<String>()),
    );
    for field in &DRAFT_FIELDS[1..] {
        let list = draft[*field]
            .as_array()
            .with_context(|| format!("local clerk field {field} is not a list"))?;
        let items: Vec<_> = list
            .iter()
            .filter_map(Value::as_str)
            .take(3)
            .map(|item| item.chars().take(160).collect::<String>())
            .collect();
        result.insert((*field).into(), json!(items));
    }
    Ok(Value::Object(result))
}

fn valid_timestamp(raw: &str) -> bool {
    static LAYOUT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt ][0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?(?:[Zz]|[+-](?:[0-9]{4}|[0-9]{2}:[0-9]{2}))$")
            .expect("valid clerk timestamp layout")
    });
    if !LAYOUT.is_match(raw) {
        return false;
    }
    if raw.len() > 128 || !raw.is_ascii() || crate::instant::parse(raw).is_none() {
        return false;
    }
    let date = &raw[..10];
    let (Ok(year), Ok(month), Ok(day)) = (
        date[..4].parse::<i64>(),
        date[5..7].parse::<u32>(),
        date[8..10].parse::<u32>(),
    ) else {
        return false;
    };
    (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && crate::civil::civil_from_days(crate::civil::days_from_civil(year, month, day))
            == (year, month, day)
}

fn bounded_value(value: &Value, depth: usize, nodes: &mut usize, bytes: &mut usize) -> Result<()> {
    *nodes += 1;
    ensure!(
        depth <= 32 && *nodes <= 16_384,
        "clerk document structural limit exceeded"
    );
    match value {
        Value::String(text) => *bytes = bytes.saturating_add(text.len()),
        Value::Array(items) => {
            for item in items {
                bounded_value(item, depth + 1, nodes, bytes)?;
            }
        }
        Value::Object(items) => {
            for (key, item) in items {
                *bytes = bytes.saturating_add(key.len());
                bounded_value(item, depth + 1, nodes, bytes)?;
            }
        }
        _ => {}
    }
    ensure!(
        *bytes <= MAX_DOCUMENT_BYTES,
        "clerk document byte limit exceeded"
    );
    Ok(())
}
