//! Runtime matrix checks use owned fixtures and never launch a model by default.
#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;

#[test]
fn default_runtime_receipt_is_persisted_and_missing_live_probes_are_not_ready() {
    let host = support::Host::new();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            host.0.to_str().unwrap(),
            "--output",
            "out",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let disk: Value =
        serde_json::from_slice(&fs::read(host.0.join("out/receipt.json")).unwrap()).unwrap();
    assert_eq!(result, disk);
    let cells = result["cells"].as_array().unwrap();
    for name in [
        "embedding-canary",
        "retriever-runtime",
        "graph-semantic-runtime",
        "launcher-real-smokes",
        "local-clerk-canary",
    ] {
        let cell = cells.iter().find(|cell| cell["cell_id"] == name).unwrap();
        assert_eq!(cell["status"], "NOT_READY", "{name}");
    }
    assert!(result["status"] != "PASS");
}

#[test]
fn malformed_graph_evidence_fails_closed_without_model_calls() {
    let host = support::Host::new();
    fs::write(host.0.join("graph.json"), b"not json").unwrap();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            host.0.to_str().unwrap(),
            "--output",
            "out",
            "--graph-evidence",
            host.0.join("graph.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let graph = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["cell_id"] == "graph-semantic-runtime")
        .unwrap();
    assert_eq!(graph["status"], "FAIL-CLOSED");
}

#[test]
fn evaluator_rejects_duplicate_cells_and_incomplete_receipts() {
    let host = support::Host::new();
    let path = host.0.join("receipt.json");
    let ids = [
        "active-state-live-claims",
        "active-state-live-claims",
        "hook-program-controls",
        "launcher-programs",
        "embedding-canary",
        "retriever-runtime",
        "graph-semantic-runtime",
        "launcher-real-smokes",
        "local-clerk-canary",
    ];
    let cells: Vec<_> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "cell_id":id,"status":"PASS","required":true,"detail":"fixture","evidence":{}
            })
        })
        .collect();
    fs::write(
        &path,
        serde_json::to_vec(
            &serde_json::json!({"schema_version":1,"status":"PASS","provenance":{},"cells":cells}),
        )
        .unwrap(),
    )
    .unwrap();
    let output = host
        .command()
        .args([
            "workspace-runtime",
            "--evaluate-receipt",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["is_valid"], false);
    assert!(result["diagnostics"].to_string().contains("duplicate"));
}

#[test]
fn clerk_opt_in_without_explicit_server_and_gpu_tool_stays_not_ready() {
    let host = support::Host::new();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            host.0.to_str().unwrap(),
            "--output",
            "out",
            "--run-clerk",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cell = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["cell_id"] == "local-clerk-canary")
        .unwrap();
    assert_eq!(cell["status"], "NOT_READY");
    assert!(cell["detail"].as_str().unwrap().contains("required"));
}

#[test]
fn live_scoring_is_incomplete_without_durable_behavior_evidence() {
    let host = support::Host::new();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            host.0.to_str().unwrap(),
            "--evaluate",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["is_valid"], false);
    assert!(result["score"].as_f64().unwrap() < 100.0);
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str().unwrap().contains("launcher-real-smokes")));
}

#[test]
fn graph_reconciliation_preserves_archive_and_other_evidence() {
    let host = support::Host::new();
    let root = host.0.to_str().unwrap();
    let initial = host
        .command()
        .env("PATH", "")
        .args(["workspace-runtime", "--root", root, "--output", "out"])
        .output()
        .unwrap();
    assert_eq!(initial.status.code(), Some(2));
    let before = fs::read(host.0.join("out/receipt.json")).unwrap();
    fs::write(host.0.join("bad-graph.json"), b"{} ").unwrap();
    let update = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            root,
            "--output",
            "out",
            "--reconcile-graph-evidence",
            host.0.join("bad-graph.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        update.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    let result: Value = serde_json::from_slice(&update.stdout).unwrap();
    assert_eq!(
        fs::read(
            result["provenance"]["pre_reconcile_receipt"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        before
    );
    let graph = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["cell_id"] == "graph-semantic-runtime")
        .unwrap();
    assert_eq!(graph["status"], "FAIL-CLOSED");
}

#[test]
fn clerk_requires_explicit_exclusive_server_assertion() {
    let host = support::Host::new();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "workspace-runtime",
            "--root",
            host.0.to_str().unwrap(),
            "--output",
            "out",
            "--run-clerk",
            "--clerk-port",
            "15432",
            "--clerk-server-pid",
            "1",
            "--nvidia-smi",
            "/missing",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let clerk = result["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["cell_id"] == "local-clerk-canary")
        .unwrap();
    assert_eq!(clerk["status"], "NOT_READY");
    assert!(clerk["detail"]
        .as_str()
        .unwrap()
        .contains("--assert-exclusive-clerk-server"));
}

#[test]
fn evaluator_requires_exact_valid_nine_cell_schema_and_consistent_status() {
    let host = support::Host::new();
    let ids = [
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
    let cells: Vec<Value> = ids.iter().map(|id| serde_json::json!({"cell_id":id,"status":"PASS","required":true,"detail":"observed","evidence":{}})).collect();
    let baseline =
        serde_json::json!({"schema_version":1,"status":"PASS","provenance":{},"cells":cells});
    let path = host.0.join("evaluation.json");
    let evaluate = |payload: &Value| {
        fs::write(&path, serde_json::to_vec(payload).unwrap()).unwrap();
        let out = host
            .command()
            .args([
                "workspace-runtime",
                "--evaluate-receipt",
                path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        (
            out.status.code(),
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        )
    };
    let (code, result) = evaluate(&baseline);
    assert_eq!(code, Some(0));
    assert_eq!(result["is_valid"], true);
    let mut variants = Vec::new();
    let mut extra = baseline.clone();
    extra["cells"].as_array_mut().unwrap().push(serde_json::json!({"cell_id":"unknown","status":"FAIL-CLOSED","required":true,"detail":"bad","evidence":{}}));
    variants.push(extra);
    for required in [
        Value::Null,
        serde_json::json!("true"),
        serde_json::json!(false),
    ] {
        let mut bad = baseline.clone();
        bad["cells"][0]["required"] = required;
        variants.push(bad);
    }
    let mut absent = baseline.clone();
    absent["cells"][0]
        .as_object_mut()
        .unwrap()
        .remove("required");
    variants.push(absent);
    let mut bad_evidence = baseline.clone();
    bad_evidence["cells"][0]["evidence"] = serde_json::json!("invalid");
    variants.push(bad_evidence);
    let mut bad_status = baseline.clone();
    bad_status["cells"][0]["status"] = serde_json::json!("UNKNOWN");
    variants.push(bad_status);
    let mut mismatch = baseline.clone();
    mismatch["cells"][0]["status"] = serde_json::json!("NOT_READY");
    variants.push(mismatch);
    let mut missing_provenance = baseline.clone();
    missing_provenance
        .as_object_mut()
        .unwrap()
        .remove("provenance");
    variants.push(missing_provenance);
    for payload in variants {
        let (code, result) = evaluate(&payload);
        assert_eq!(code, Some(2), "{payload}");
        assert_eq!(result["is_valid"], false, "{payload}");
    }
}
