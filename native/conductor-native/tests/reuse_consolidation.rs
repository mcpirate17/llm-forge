use conductor_native::reuse_consolidation::decide;
use serde_json::{json, Value};

fn cluster(
    id: &str,
    batch: i64,
    disposition: &str,
    tokens: i64,
    value: i64,
    files: &[&str],
) -> Value {
    let sites = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            json!({"file":file, "line_start":index + 3, "line_end":index + 8,
                "name":"clone", "node_hash":"shape", "tokens":tokens,
                "source":"return item"})
        })
        .collect::<Vec<_>>();
    json!({"kind":"exact", "tokens":tokens, "sites":sites,
        "id":id, "batch":batch, "confidence":0.755,
        "value_score":value, "risk":"low", "disposition":disposition,
        "rationale":"exact, same-dir"})
}

#[test]
fn token_clone_ingest_preserves_order_and_lines_fallback() {
    let rows = decide(
        "token_clones",
        &json!({"duplicates":[
            {"tokens":0, "lines":12,
             "firstFile":{"name":"pkg/a.py","start":3,"end":8},
             "secondFile":{"name":"pkg/b.py","start":4,"end":9}},
            {"tokens":80,
             "firstFile":{"name":"pkg/c.py","start":1,"end":5},
             "secondFile":{"name":"pkg/d.py","start":2,"end":6}},
            {"tokens":99,"firstFile":{},"secondFile":{"name":"ignored"}}
        ]}),
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["kind"], "token");
    assert_eq!(rows[0]["tokens"], 12);
    assert_eq!(rows[0]["sites"][0]["file"], "pkg/a.py");
    assert_eq!(rows[0]["sites"][1]["line_start"], 4);
    assert_eq!(rows[0]["sites"][1]["name"], "<clone>");
    assert_eq!(rows[1]["tokens"], 80);
    assert_eq!(rows[1]["sites"][0]["file"], "pkg/c.py");
}

#[test]
fn output_rows_and_summary_keep_public_shape_and_actionable_scope() {
    let clusters = json!([
        cluster("C001", 0, "auto", 20, 40, &["pkg/a.py", "pkg/b.py"]),
        cluster("C002", 0, "auto", 10, 30, &["pkg/c.py", "pkg/d.py"]),
        cluster("C003", -1, "validate", 7, 12, &["pkg/e.py", "pkg/f.py"]),
        cluster("C004", -1, "ignore", 5, 3, &["pkg/g.py", "pkg/h.py"])
    ]);
    let payload = json!({"clusters":clusters, "homes":["pkg/_common.py",null,null,null],
        "files_scanned":8,"files_unparsable":1,"functions_considered":16});
    let rows = decide("cluster_dicts", &payload).unwrap();
    assert_eq!(rows[0]["id"], "C001");
    assert_eq!(rows[0]["n_sites"], 2);
    assert_eq!(rows[0]["est_bytes"], 20);
    assert_eq!(rows[0]["suggested_home"], "pkg/_common.py");
    assert_eq!(
        rows[0]["sites"],
        json!([
            {"file":"pkg/a.py","line_start":3,"line_end":8,"name":"clone"},
            {"file":"pkg/b.py","line_start":4,"line_end":9,"name":"clone"}
        ])
    );
    assert_eq!(rows[2]["suggested_home"], Value::Null);
    let summary = decide("report_summary", &payload).unwrap();
    assert_eq!(summary["n_clusters"], 4);
    assert_eq!(summary["n_actionable"], 2);
    assert_eq!(summary["n_validate"], 1);
    assert_eq!(summary["n_ignored"], 1);
    assert_eq!(summary["n_batches"], 1);
    assert_eq!(summary["total_redundant_bytes"], 42);
    assert_eq!(summary["actionable_redundant_bytes"], 30);
    assert_eq!(summary["actionable_value_score"], 70);
    assert_eq!(summary["files_scanned"], 8);
    assert_eq!(summary["files_unparsable"], 1);
    assert_eq!(summary["functions_considered"], 16);
    let mut bad = payload;
    bad["homes"] = json!([]);
    assert_eq!(
        decide("cluster_dicts", &bad).unwrap_err(),
        "consolidation homes must match clusters"
    );
}

#[test]
fn markdown_preserves_table_text_float_rounding_and_six_site_cap() {
    let files = [
        "p/a.py", "p/b.py", "p/c.py", "p/d.py", "p/e.py", "p/f.py", "p/g.py",
    ];
    let payload = json!({"clusters":[cluster("C001",0,"auto",20,40,&files)],
        "homes":[null], "files_scanned":7,"files_unparsable":0,
        "functions_considered":7});
    let rows = decide("cluster_dicts", &payload).unwrap();
    let row = decide("markdown_row", &rows[0]).unwrap();
    assert_eq!(row, "| C001 | 0 | exact | auto | 0.76 | 40 | 7 | 120 | review required | p/a.py:3, p/b.py:4, p/c.py:5, p/d.py:6, p/e.py:7, p/f.py:8, +1 more |");
    let summary = decide("report_summary", &payload).unwrap();
    let markdown = decide(
        "markdown",
        &json!({
            "generated_at":"2026-09-27T00:00:00+00:00",
            "summary":summary, "clusters":rows
        }),
    )
    .unwrap();
    let text = markdown.as_str().unwrap();
    assert!(text.starts_with("# Consolidation report — generated 2026-09-27T00:00:00+00:00\n\n1 clusters across 1 batches; 1 auto-actionable, 0 require validation; ~120 redundant AST-node-units; 7 files scanned (0 unparsable), 7 functions considered.\n\n"));
    assert!(text.contains("| id | batch | kind | disposition | confidence | value | n_sites | est_bytes | suggested_home | sites (file:line, ...) |\n"));
    assert!(text.ends_with("+1 more |\n"));
}
