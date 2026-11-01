use super::*;
use serde_json::json;
fn normalized(command: Vec<String>, version: Vec<String>) -> Value {
    conductor_native::candidate_policy::fragment(
        "check",
        &json!({"id":"lint","value":{
        "kind":"command","profiles":["fast"],"command":command,"version_command":version}}),
        "2026-11-01",
    )
    .unwrap()
}
fn check(command: &[&str], version: &[&str]) -> ExternalCheck {
    ExternalCheck::from_normalized(&normalized(
        command.iter().map(|s| (*s).into()).collect(),
        version.iter().map(|s| (*s).into()).collect(),
    ))
    .unwrap()
    .unwrap()
}
fn candidate() -> Candidate {
    serde_json::from_value(
        json!({"kind":"index","tree_oid":"tree-oid","base_tree_oid":"base-tree",
        "base_commit_oid":"base-commit","commit_oid":null,"target_ref":null,"changes":[],
        "integration_base_oid":null,"integration_base_detail":""}),
    )
    .unwrap()
}
fn inputs() -> OperatorInputs<'static> {
    OperatorInputs {
        repo: Path::new("/operator/repo"),
        snapshot: Path::new("/operator/snapshot"),
        runtime_dir: Path::new("/operator/runtime"),
        python: Some("/operator/granted python"),
    }
}

#[test]
fn placeholders_expand_in_legacy_order_without_shell_or_embedded_files_expansion() {
    let check = check(
        &[
            "tool",
            "{repo}",
            "prefix:{snapshot}:{base}:{tree}",
            "{python}",
            "{files}",
            "--input={files}",
            "{unknown}",
            "; $(id) `uname`",
        ],
        &["tool", "--version"],
    );
    let files = vec!["src/a b.py".into(), "src/c.rs".into()];
    let prepared = prepare(&check, &candidate(), &inputs(), &files).unwrap();
    assert_eq!(
        prepared.command.argv,
        vec![
            "tool",
            "/operator/repo",
            "prefix:/operator/snapshot:base-commit:tree-oid",
            "/operator/granted python",
            "src/a b.py",
            "src/c.rs",
            "--input={files}",
            "{unknown}",
            "; $(id) `uname`"
        ]
    );
    assert!(prepared.command.changed_files.is_none());
}

#[test]
fn ordered_replacements_cascade_once_and_changed_list_is_only_original_template_triggered() {
    let mut candidate = candidate();
    candidate.base_commit_oid = None;
    candidate.base_tree_oid = "{tree}".into();
    candidate.tree_oid = "{python}".into();
    let input = OperatorInputs {
        repo: Path::new("{snapshot}"),
        snapshot: Path::new("{base}"),
        runtime_dir: Path::new("/operator/runtime"),
        python: Some("/granted/{changed_files_file}"),
    };
    let a = check(&["tool", "{repo}"], &["tool"]);
    let prepared = prepare(&a, &candidate, &input, &[]).unwrap();
    assert_eq!(prepared.command.argv[1], "/granted/{changed_files_file}");
    assert!(prepared.command.changed_files.is_none());
    let b = check(&["tool", "{repo}", "{changed_files_file}"], &["tool"]);
    let prepared = prepare(&b, &candidate, &input, &[]).unwrap();
    assert_eq!(
        prepared.command.argv[1],
        "/granted//operator/runtime/changed-files-lint.txt"
    );
    let input = OperatorInputs {
        repo: Path::new("{repo}"),
        ..inputs()
    };
    let prepared = prepare(&a, &candidate, &input, &[]).unwrap();
    assert_eq!(prepared.command.argv[1], "{repo}");
}

#[test]
fn changed_file_contents_preserve_order_duplicates_and_version_uses_empty_files() {
    let check = check(
        &["tool", "--list={changed_files_file}", "{files}"],
        &["tool", "{files}", "prefix{files}", "{changed_files_file}"],
    );
    let files = vec!["src/a b.py".into(), "src/c.rs".into(), "src/a b.py".into()];
    let prepared = prepare(&check, &candidate(), &inputs(), &files).unwrap();
    let changed = prepared.command.changed_files.unwrap();
    assert_eq!(
        changed.path,
        Path::new("/operator/runtime/changed-files-lint.txt")
    );
    assert_eq!(changed.bytes, b"src/a b.py\nsrc/c.rs\nsrc/a b.py\n");
    assert_eq!(
        prepared.version.argv,
        vec![
            "tool",
            "prefix{files}",
            "/operator/runtime/changed-files-lint.txt"
        ]
    );
    let version = prepared.version.changed_files.unwrap();
    assert_eq!(version.path, changed.path);
    assert!(version.bytes.is_empty());
}

#[test]
fn base_commit_precedes_tree_but_none_and_empty_commit_use_base_tree() {
    let check = check(&["tool", "{base}"], &["tool"]);
    let mut model = candidate();
    assert_eq!(
        prepare(&check, &model, &inputs(), &[])
            .unwrap()
            .command
            .argv[1],
        "base-commit"
    );
    for value in [None, Some(String::new())] {
        model.base_commit_oid = value;
        assert_eq!(
            prepare(&check, &model, &inputs(), &[])
                .unwrap()
                .command
                .argv[1],
            "base-tree"
        );
    }
}

#[test]
fn files_spliced_as_whole_arguments_are_never_substituted() {
    let splice = check(&["tool", "{files}"], &["tool"]);
    let prepared = prepare(&splice, &candidate(), &inputs(), &["src/{repo}.py".into()]).unwrap();
    assert_eq!(prepared.command.argv, vec!["tool", "src/{repo}.py"]);
}

#[test]
fn python_placeholder_requires_operator_input_without_path_or_interpreter_discovery() {
    let input = OperatorInputs {
        python: None,
        ..inputs()
    };
    let ordinary = check(
        &["native-tool", "{snapshot}"],
        &["native-tool", "--version"],
    );
    assert!(prepare(&ordinary, &candidate(), &input, &[]).is_ok());
    let python = check(&["{python}", "check"], &["native-tool"]);
    assert!(prepare(&python, &candidate(), &input, &[])
        .unwrap_err()
        .to_string()
        .contains("operator-granted"));
}

#[test]
fn ambiguous_file_lists_nul_and_unsafe_artifact_components_are_refused() {
    let check = check(&["tool", "{changed_files_file}"], &["tool"]);
    for file in [
        "src/new\nline.py",
        "src/new\rline.py",
        "src/nul\0.py",
        "../outside.py",
        "/absolute.py",
    ] {
        assert!(
            prepare(&check, &candidate(), &inputs(), &[file.into()]).is_err(),
            "{file:?}"
        );
    }
    let mut unsafe_id = check.clone();
    unsafe_id.check_id = "../escape".into();
    assert!(prepare(&unsafe_id, &candidate(), &inputs(), &[]).is_err());
    let input = OperatorInputs {
        repo: Path::new("bad\0path"),
        ..inputs()
    };
    assert!(prepare(&check, &candidate(), &input, &[]).is_err());
    let mut value = normalized(vec!["tool".into(), "bad\0arg".into()], vec!["tool".into()]);
    assert!(ExternalCheck::from_normalized(&value).is_err());
    value["command"] = json!(["tool", 17]);
    assert!(ExternalCheck::from_normalized(&value).is_err());
}

#[test]
fn empty_entries_cannot_bypass_count_admission_and_files_cannot_overflow_argv() {
    let mut value = normalized(vec!["tool".into()], vec!["tool".into()]);
    value["command"] = json!(vec![""; MAX_ARG_COUNT + 1]);
    assert!(ExternalCheck::from_normalized(&value)
        .unwrap_err()
        .to_string()
        .contains("count"));
    let splice = check(&["tool", "{files}"], &["tool"]);
    let files = (0..MAX_ARG_COUNT + 1)
        .map(|n| format!("src/{n}.py"))
        .collect::<Vec<_>>();
    assert!(prepare(&splice, &candidate(), &inputs(), &files)
        .unwrap_err()
        .to_string()
        .contains("count"));
    let no_executable = check(&["{files}"], &["tool"]);
    assert!(prepare(&no_executable, &candidate(), &inputs(), &[])
        .unwrap_err()
        .to_string()
        .contains("executable"));
}

#[test]
fn byte_amplification_vector_bytes_and_work_are_bounded_before_allocation() {
    let mut value = normalized(vec!["tool".into()], vec!["tool".into()]);
    value["command"] = json!(vec!["x".repeat(MAX_TEMPLATE_ARG); 257]);
    assert!(ExternalCheck::from_normalized(&value)
        .unwrap_err()
        .to_string()
        .contains("byte limit"));
    let expanded = normalized(
        vec!["tool".into(), "{repo}".repeat(512)],
        vec!["tool".into()],
    );
    let check = ExternalCheck::from_normalized(&expanded).unwrap().unwrap();
    let large = "x".repeat(4096);
    let input = OperatorInputs {
        repo: Path::new(&large),
        ..inputs()
    };
    assert!(prepare(&check, &candidate(), &input, &[])
        .unwrap_err()
        .to_string()
        .contains("argument byte"));
    let expanded = normalized(
        std::iter::once("tool".into())
            .chain(vec!["{repo}".into(); 1024])
            .collect(),
        vec!["tool".into()],
    );
    let check = ExternalCheck::from_normalized(&expanded).unwrap().unwrap();
    let large = "x".repeat(2000);
    let input = OperatorInputs {
        repo: Path::new(&large),
        ..inputs()
    };
    assert!(prepare(&check, &candidate(), &input, &[])
        .unwrap_err()
        .to_string()
        .contains("argv exceeds"));
    assert!(prepare_with_work(&check, &candidate(), &inputs(), &[], 32)
        .unwrap_err()
        .to_string()
        .contains("work limit"));
}

#[test]
fn external_output_admission_is_bounded_even_though_core_policy_has_no_upper_cap() {
    let mut value = normalized(vec!["tool".into()], vec!["tool".into()]);
    value["max_output_chars"] = json!(MAX_OUTPUT_CHARS);
    assert_eq!(
        ExternalCheck::from_normalized(&value)
            .unwrap()
            .unwrap()
            .max_output_chars,
        MAX_OUTPUT_CHARS
    );
    for invalid in [
        json!(0),
        json!(MAX_OUTPUT_CHARS + 1),
        json!(true),
        json!("12000"),
        json!(u64::MAX),
    ] {
        value["max_output_chars"] = invalid;
        assert!(ExternalCheck::from_normalized(&value).is_err());
    }
}

#[test]
fn active_checks_reuse_policy_profile_order_and_skip_builtins() {
    let a = normalized(vec!["tool-a".into()], vec!["tool-a".into()]);
    let mut b = normalized(vec!["tool-b".into()], vec!["tool-b".into()]);
    b["check_id"] = json!("second");
    b["profiles"] = json!(["full"]);
    let mut c = normalized(vec!["tool-c".into()], vec!["tool-c".into()]);
    c["check_id"] = json!("third");
    let builtin = conductor_native::candidate_policy::fragment(
        "check",
        &json!({"id":"internal",
        "value":{"kind":"builtin","profiles":["fast"]}}),
        "2026-11-01",
    )
    .unwrap();
    let policy = Policy {
        path: "candidate_policy.toml".into(),
        digest: "fixture".into(),
        document: json!({"checks":[a,c,b,builtin]}),
    };
    let fast = selected_external(&policy, "fast").unwrap();
    assert_eq!(fast.len(), 2);
    assert_eq!(fast[0].command, vec!["tool-a"]);
    assert_eq!(fast[1].command, vec!["tool-c"]);
    let full = selected_external(&policy, "full").unwrap();
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].command, vec!["tool-b"]);
    assert!(selected_external(&policy, "unknown").is_err());
}
