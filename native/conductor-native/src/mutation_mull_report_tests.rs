use super::*;
use serde_json::json;

const SOURCE: &str = "int f(int x) { return x + 1; }\n";
fn mutant(id: &str, status: &str) -> Value {
    json!({"id":id,"mutatorName":"cxx_add_to_sub","replacement":"-","status":status,
        "location":{"start":{"line":1,"column":25},"end":{"line":1,"column":26}}})
}
fn report(path: &Path) -> Value {
    json!({"schemaVersion":"1.7","config":{"Mull Version":"0.34.0"},"files":{(path.to_str().unwrap()):{"source":SOURCE,"mutants":[mutant("a","Killed"),mutant("b","Survived")]}}})
}
fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    (
        "/private/worktree".into(),
        "/private/worktree/src/a.cpp".into(),
    )
}

#[test]
fn valid_report_preserves_selected_repo_path_and_outcomes() {
    let (root, source) = fixture();
    let rows = rows(&report(&source), &root, &source, SOURCE).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["path"] == "src/a.cpp"));
    assert_eq!(rows[0]["outcome"], "KILLED");
    assert_eq!(rows[1]["outcome"], "SURVIVED");
    assert_eq!(rows[0]["original_text"], "+");
}
#[test]
fn missing_duplicate_wrong_scope_and_unscored_reports_fail_closed() {
    let (root, source) = fixture();
    let original = report(&source);
    let key = source.to_str().unwrap();
    for case in [
        "missing",
        "duplicate",
        "wrong_source",
        "extra_file",
        "empty",
        "uncovered",
        "unknown",
        "missing_id",
    ] {
        let mut bad = original.clone();
        match case {
            "missing" => {
                bad.as_object_mut().unwrap().remove("files");
            }
            "duplicate" => bad["files"][key]["mutants"][1]["id"] = json!("a"),
            "wrong_source" => bad["files"][key]["source"] = json!("different"),
            "extra_file" => bad["files"]["/unselected.cpp"] = json!({"source":"","mutants":[]}),
            "empty" => bad["files"][key]["mutants"] = json!([]),
            "uncovered" => bad["files"][key]["mutants"] = json!([mutant("a", "NoCoverage")]),
            "unknown" => bad["files"][key]["mutants"][0]["status"] = json!("Pending"),
            "missing_id" => {
                bad["files"][key]["mutants"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("id");
            }
            _ => unreachable!(),
        }
        assert!(rows(&bad, &root, &source, SOURCE).is_err(), "{case}");
    }
}
#[test]
fn invalid_locations_and_unsafe_byte_character_mapping_refuse() {
    let (root, source) = fixture();
    let key = source.to_str().unwrap();
    for location in [
        json!({}),
        json!({"start":{"line":0,"column":1},"end":{"line":1,"column":2}}),
        json!({"start":{"line":1,"column":25},"end":{"line":1,"column":24}}),
        json!({"start":{"line":2,"column":1},"end":{"line":2,"column":2}}),
        json!({"start":{"line":1,"column":900},"end":{"line":1,"column":901}}),
    ] {
        let mut bad = report(&source);
        bad["files"][key]["mutants"][0]["location"] = location;
        assert!(rows(&bad, &root, &source, SOURCE).is_err());
    }
    let unicode = "// é + 1\n";
    let mut bad = report(&source);
    bad["files"][key]["source"] = json!(unicode);
    bad["files"][key]["mutants"] = json!([mutant("a", "Killed")]);
    bad["files"][key]["mutants"][0]["location"] =
        json!({"start":{"line":1,"column":7},"end":{"line":1,"column":8}});
    assert!(rows(&bad, &root, &source, unicode).is_err());
}
