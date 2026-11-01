use super::PathSuffixes;
use crate::{
    candidate_checks::select_files,
    candidate_policy::{classify, fragment},
};
use serde_json::{json, Value};

const CPP: [&str; 10] = [
    ".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx", ".cu", ".cuh",
];
fn check(suffixes: Value) -> Value {
    json!({"classes":["native"],"exclude_classes":["generated"],"run_on_deletions":false,
        "path_suffixes":suffixes})
}
fn change(path: &str) -> Value {
    json!({"path":path,"old_path":null,"new_mode":"100644","classes":["native"]})
}
fn normalize(suffixes: Option<Value>) -> Result<Value, String> {
    let mut raw = json!({"kind":"command","profiles":["full"],"classes":["native"],
        "command":["cppcheck","{files}"],"version_command":["cppcheck","--version"]});
    if let Some(value) = suffixes {
        raw["path_suffixes"] = value;
    }
    fragment("check", &json!({"id":"cppcheck","value":raw}), "2026-11-01")
}

#[test]
fn all_ten_cpp_suffixes_match_case_insensitively_without_changing_native_membership() {
    let changes: Vec<Value> = CPP
        .iter()
        .enumerate()
        .map(|(i, suffix)| change(&format!("src/f{i}{}", suffix.to_ascii_uppercase())))
        .collect();
    let paths = select_files(&changes, &check(json!(CPP))).unwrap();
    assert_eq!(paths.len(), 10);
    assert!(paths.contains(&"src/f5.HH"));
    assert!(paths.contains(&"src/f7.HXX"));
    let mut nonnative = change("src/not-native.hh");
    nonnative["classes"] = json!(["source"]);
    assert!(select_files(&[nonnative], &check(json!(CPP)))
        .unwrap()
        .is_empty());
}

#[test]
fn current_suffix_does_not_follow_old_rename_class_or_custom_native_txt() {
    let globs =
        json!({"class_globs":{"native":["custom/*"]},"generated_globs":[],"high_risk_globs":[]});
    let mut changes = Vec::new();
    for path in [
        "src/new.rs",
        "Cargo.toml",
        "custom/readme.txt",
        "src/yes.CPP",
    ] {
        let mut c = change(path);
        c["old_path"] = json!("src/old.c");
        c["old_mode"] = json!("100644");
        c["classes"] = classify(&c, &globs).unwrap()["classes"].clone();
        changes.push(c);
    }
    assert_eq!(
        select_files(&changes, &check(json!(CPP))).unwrap(),
        ["src/yes.CPP"]
    );
}

#[test]
fn basename_only_final_suffix_and_dotfile_rules_are_exact() {
    let suffixes = PathSuffixes::new(Some(&json!(CPP))).unwrap();
    for path in [
        "root.HH",
        "nested/root.hXx",
        ".foo.c",
        "nested/.foo.C",
        "many.part.cpp",
    ] {
        assert!(suffixes.matches(path), "{path}");
    }
    for path in [
        ".c",
        "nested/.CPP",
        "file.c.",
        "dir.c/name",
        "f.cpp.bak",
        "file",
        "file.",
    ] {
        assert!(!suffixes.matches(path), "{path}");
    }
    let suffixes = PathSuffixes::new(Some(&json!([".a_+-1"]))).unwrap();
    assert!(suffixes.matches("file.A_+-1"));
}

#[test]
fn existing_deletion_exclusion_and_sorted_dedup_semantics_survive() {
    let mut deleted = change("gone.c");
    deleted["new_mode"] = json!("000000");
    let mut generated = change("gen.c");
    generated["classes"] = json!(["native", "generated"]);
    let changes = vec![
        change("z.C"),
        deleted,
        change("a.c"),
        generated,
        change("z.C"),
        change("native.rs"),
    ];
    let mut policy = check(json!(CPP));
    assert_eq!(select_files(&changes, &policy).unwrap(), ["a.c", "z.C"]);
    policy["run_on_deletions"] = json!(true);
    assert_eq!(
        select_files(&changes, &policy).unwrap(),
        ["a.c", "gone.c", "z.C"]
    );
}

#[test]
fn absent_and_empty_are_unrestricted_and_normalization_is_lowercase_ordered() {
    assert_eq!(normalize(None).unwrap()["path_suffixes"], json!([]));
    assert_eq!(
        normalize(Some(json!([]))).unwrap()["path_suffixes"],
        json!([])
    );
    assert_eq!(
        normalize(Some(json!([".CPP", ".Hh", ".c"]))).unwrap()["path_suffixes"],
        json!([".cpp", ".hh", ".c"])
    );
    let changes = vec![change("a.rs"), change(".c")];
    let mut policy = check(json!([]));
    assert_eq!(select_files(&changes, &policy).unwrap(), [".c", "a.rs"]);
    policy.as_object_mut().unwrap().remove("path_suffixes");
    assert_eq!(select_files(&changes, &policy).unwrap(), [".c", "a.rs"]);
}

#[test]
fn malformed_suffix_admission_is_shared_and_runs_even_for_empty_changes() {
    let mut invalid = vec![
        Value::Null,
        json!(".c"),
        json!([null]),
        json!([1]),
        json!([""]),
        json!(["."]),
        json!(["c"]),
        json!([".tar.gz"]),
        json!([".*"]),
        json!([".c/xx"]),
        json!([".c\\xx"]),
        json!([".é"]),
        json!([".c\n"]),
        json!([".c\u{0}"]),
        json!([".C", ".c"]),
        json!([format!(".{}", "a".repeat(32))]),
    ];
    invalid.push(json!((0..65).map(|i| format!(".s{i}")).collect::<Vec<_>>()));
    for value in invalid {
        assert!(normalize(Some(value.clone())).is_err(), "{value}");
        assert!(select_files(&[], &check(value.clone())).is_err(), "{value}");
    }
    let max = json!((0..64).map(|i| format!(".s{i}")).collect::<Vec<_>>());
    assert!(normalize(Some(max)).is_ok());
    assert!(normalize(Some(json!([format!(".{}", "a".repeat(31))]))).is_ok());
}
