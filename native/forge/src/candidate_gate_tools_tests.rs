use super::*;
fn tool(id: &str, found: bool, expected: &str, matches: bool) -> ToolStatus {
    ToolStatus {
        tool_id: id.into(),
        executable: id.into(),
        found,
        resolved_path: None,
        version: None,
        expected_version: expected.into(),
        matches_expected: matches,
        provided_by: "operator".into(),
    }
}
#[test]
fn missing_has_priority_over_version_drift_and_empty_pins_are_not_enforced() {
    let status = vec![
        tool("drift", true, "1.0", false),
        tool("missing", false, "", false),
    ];
    let phase = summarize(&status);
    assert!(!phase.ok);
    assert_eq!(phase.evidence["missing"], json!(["missing"]));
    let phase = summarize(&[tool("drift", true, "1.0", false)]);
    assert!(!phase.ok);
    assert_eq!(phase.evidence["drifted"], json!(["drift"]));
    assert!(summarize(&[tool("unpinned", true, "", false)]).ok);
}
