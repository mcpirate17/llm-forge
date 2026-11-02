use super::*;
fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).into()).collect()
}
#[test]
fn measured_territory_covers_nested_language_files_but_excludes_deleted_and_pinned() {
    let campaigns = vec![CampaignScope {
        language: "rust".into(),
        source_paths: vec!["native/src/lib.rs".into()],
        test_paths: vec![],
    }];
    let changed: Vec<_> = [
        "native/src/lib.rs",
        "native/src/new.rs",
        "native/src/sub/new.rs",
        "native/src/doc.md",
        "native/src/deleted.rs",
    ]
    .iter()
    .map(|value| (*value).into())
    .collect();
    let result = uncovered(
        &changed,
        &set(&[
            "native/src/lib.rs",
            "native/src/new.rs",
            "native/src/sub/new.rs",
            "native/src/doc.md",
        ]),
        &campaigns,
    )
    .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|row| row.file.as_str())
            .collect::<Vec<_>>(),
        vec!["native/src/new.rs", "native/src/sub/new.rs"]
    );
}
#[test]
fn unknown_language_only_covers_exact_parent_and_invalid_paths_refuse() {
    let campaigns = vec![CampaignScope {
        language: "unknown".into(),
        source_paths: vec!["a/pinned.xyz".into()],
        test_paths: vec![],
    }];
    let changed = vec!["a/new.txt".into(), "a/sub/child.txt".into()];
    let result = uncovered(
        &changed,
        &set(&["a/new.txt", "a/sub/child.txt"]),
        &campaigns,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "a/new.txt");
    assert!(uncovered(&["../escape.py".into()], &BTreeSet::new(), &campaigns).is_err());
}
