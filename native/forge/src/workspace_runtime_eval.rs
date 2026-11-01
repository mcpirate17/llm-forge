//! Bounded completeness evaluation of persisted runtime receipts.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const REQUIRED: [&str; 9] = [
    "active-state-live-claims",
    "hook-config-contract",
    "hook-program-controls",
    "launcher-programs",
    "embedding-canary",
    "retriever-runtime",
    "graph-semantic-runtime",
    "launcher-real-smokes",
    "local-clerk-canary",
];

pub(super) fn receipt(path: &Path) -> Value {
    let result = (|| -> anyhow::Result<Value> {
        let bytes = forge::bounded_file::read(path, 1024 * 1024)?;
        let payload: Value = forge::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?;
        super::reconcile::validate(&payload)?;
        let cells = payload["cells"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("receipt cells missing"))?;
        let mut problems = Vec::new();
        for id in REQUIRED {
            match cells.iter().find(|row| row["cell_id"] == id) {
                None => problems.push(format!("missing runtime cell: {id}")),
                Some(row) if row["status"] != "PASS" || row["required"] != true => {
                    problems.push(format!("non-PASS runtime cell: {id}"))
                }
                _ => (),
            }
        }
        if payload["status"] != "PASS" {
            problems.push("receipt overall status is not PASS".to_string());
        }
        let valid = problems.is_empty();
        Ok(
            json!({"status":if valid {"PASS"} else {"FAIL-CLOSED"},"is_valid":valid,
            "score":if valid {100.0} else {0.0},"improved":false,
            "metrics":{"runtime_receipt":{"path":path,"sha256":format!("{:x}",Sha256::digest(&bytes)),
            "provenance":payload.get("provenance")}},"diagnostics":problems}),
        )
    })();
    result.unwrap_or_else(|error| json!({"status":"FAIL-CLOSED","is_valid":false,"score":0.0,
        "improved":false,"metrics":{},"diagnostics":[format!("runtime receipt unreadable or malformed: {error:#}")]}))
}

pub(super) fn live(
    cells: &[Value],
    status: Value,
    seed: i64,
    elapsed: std::time::Duration,
) -> Value {
    let passed = cells.iter().filter(|cell| cell["status"] == "PASS").count();
    let diagnostics: Vec<_> = cells
        .iter()
        .filter(|cell| cell["status"] != "PASS")
        .map(|cell| {
            format!(
                "{}: {}",
                cell["cell_id"].as_str().unwrap_or("?"),
                cell["detail"].as_str().unwrap_or("")
            )
        })
        .collect();
    let checks: serde_json::Map<String, Value> = cells
        .iter()
        .filter_map(|cell| {
            cell["cell_id"]
                .as_str()
                .map(|id| (id.to_owned(), cell.clone()))
        })
        .collect();
    let score = if cells.is_empty() {
        0.0
    } else {
        (100_000.0 * passed as f64 / cells.len() as f64).round() / 1000.0
    };
    json!({"status":status,"is_valid":false,"score":score,"improved":false,
        "elapsed_seconds":(elapsed.as_secs_f64()*1000.0).round()/1000.0,"mode":"workspace",
        "metrics":{"checks":checks,"provenance":{"seed":seed,"config":"forge.workspace-runtime","oracle":"native runtime matrix"}},"diagnostics":diagnostics})
}
