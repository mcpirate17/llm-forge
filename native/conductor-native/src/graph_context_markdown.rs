//! Compact Markdown context rendering with exact role and count rules.

use super::GraphRelationship;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Deserialize)]
struct Summary {
    file_path: String,
    skeleton: String,
    graph_status: String,
    callers: Vec<GraphRelationship>,
    callees: Vec<GraphRelationship>,
}

pub(super) fn is_test_path(path: &str) -> bool {
    let components: Vec<String> = Path::new(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some(name) = components.last() else {
        return false;
    };
    name.starts_with("test_")
        || name.ends_with("_test.py")
        || components[..components.len() - 1]
            .iter()
            .any(|part| part == "tests")
}

fn grouped(lines: &mut Vec<String>, heading: &str, items: &[String]) {
    if !items.is_empty() {
        lines.push(format!("\n**{heading}:**"));
        for item in items.iter().take(15) {
            lines.push(format!("- {item}"));
        }
    }
}

pub(super) fn format(input: &Value) -> Result<String, String> {
    let summary: Summary = serde_json::from_value(input.clone())
        .map_err(|error| format!("invalid graph context summary: {error}"))?;
    let mut lines = vec![
        format!("### AST Context: `{}`", summary.file_path),
        "```python".to_owned(),
        summary.skeleton.trim().to_owned(),
        "```".to_owned(),
    ];
    if summary.graph_status.starts_with("unavailable") {
        lines.push(format!(
            "\n*Notice: code-review-graph {}*",
            summary.graph_status
        ));
    }
    let mut calls = Vec::new();
    let mut called_by = Vec::new();
    let mut tested_by = Vec::new();
    for item in &summary.callers {
        let label = format!("`{}`", item.qualified_name);
        if item.kind.to_lowercase().contains("test") || is_test_path(&item.file_path) {
            tested_by.push(label);
        } else {
            called_by.push(format!("{label} ({})", item.kind));
        }
    }
    for item in &summary.callees {
        let label = format!("`{}`", item.qualified_name);
        if item.kind.to_lowercase().contains("test") || is_test_path(&item.file_path) {
            tested_by.push(label);
        } else {
            calls.push(format!("{label} ({})", item.kind));
        }
    }
    grouped(&mut lines, "Called By (Inbound Call Sites)", &called_by);
    grouped(&mut lines, "Calls (Outbound Dependencies)", &calls);
    if !tested_by.is_empty() {
        lines.push("\n**Tested By (Test Suites / Invariants):**".to_owned());
        for item in tested_by.into_iter().take(15).collect::<BTreeSet<_>>() {
            lines.push(format!("- {item}"));
        }
    }
    Ok(format!("{}\n", lines.join("\n")))
}
