use super::*;
fn file(path: &str, lines: &[u32], high_risk: bool) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        absolute_path: format!("/owned/snapshot/{path}"),
        lines: lines.iter().copied().collect(),
        high_risk,
    }
}
#[test]
fn actual_lines_are_scored_by_risk() {
    let json = br#"{"files":{"a.py":{"executed_lines":[1,3],"missing_lines":[2]},"b.py":{"executed_lines":[4],"missing_lines":[5]}}}"#;
    let result = evaluate(
        json,
        &[file("a.py", &[1, 2, 3], true), file("b.py", &[4, 5], false)],
        90.,
        50.,
    )
    .unwrap();
    assert_eq!((result.high.covered, result.high.measurable), (2, 3));
    assert!(!result.high_pass);
    assert!(result.other_pass);
}
#[test]
fn missing_or_ambiguous_data_refuses() {
    let changed = [file("a.py", &[1], false)];
    assert!(evaluate(br#"{"files":{}}"#, &changed, 80., 80.).is_err());
    assert!(evaluate(
        br#"{"files":{"a.py":{"executed_lines":[1],"missing_lines":[1]}}}"#,
        &changed,
        80.,
        80.
    )
    .is_err());
    assert!(evaluate(br#"{"files":{"a.py":{"executed_lines":[],"missing_lines":[]},"/owned/snapshot/a.py":{"executed_lines":[],"missing_lines":[]}}}"#, &changed,80.,80.).is_err());
    assert!(evaluate(br#"{"files":{},"files":{}}"#, &changed, 80., 80.).is_err());
}
#[test]
fn shard_union_counts_hits_from_any_shard_once() {
    let shards = vec![
        (
            br#"{"files":{"a.py":{"executed_lines":[1],"missing_lines":[2]}}}"#.to_vec(),
            "/owned/one".into(),
        ),
        (
            br#"{"files":{"/owned/two/a.py":{"executed_lines":[2],"missing_lines":[1]}}}"#.to_vec(),
            "/owned/two".into(),
        ),
    ];
    let result = evaluate_shards(&shards, &[file("a.py", &[1, 2], true)], 100., 80.).unwrap();
    assert_eq!((result.high.covered, result.high.measurable), (2, 2));
    assert!(result.high_pass);
    assert!(evaluate_shards(&[], &[], 80., 80.).is_err());
}
