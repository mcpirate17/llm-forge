use super::*;
fn file() -> ChangedFile {
    ChangedFile {
        path: "src/lib.rs".into(),
        absolute_path: String::new(),
        lines: [3, 4].into_iter().collect(),
        high_risk: false,
    }
}
#[test]
fn lcov_uses_captured_paths_and_unions_duplicate_source_records() {
    let data=b"TN:\nSF:/owned/snapshot/src/lib.rs\nDA:3,1\nDA:4,0\nLF:2\nLH:1\nend_of_record\nTN:\nSF:/owned/snapshot/src/lib.rs\nDA:4,2\nend_of_record\n";
    let result = evaluate(data, "/owned/snapshot", &[file()], 90., 100.).unwrap();
    assert_eq!((result.other.covered, result.other.measurable), (2, 2));
    assert!(result.other_pass);
}
#[test]
fn absent_truncated_traversal_and_invalid_counts_are_refused() {
    for data in [
        b"TN:\nSF:/other/src/lib.rs\nDA:3,1\nend_of_record\n".as_slice(),
        b"SF:/owned/snapshot/src/lib.rs\nDA:3,1\n",
        b"SF:../escape\nDA:3,1\nend_of_record\n",
        b"SF:src/lib.rs\nDA:3,-1\nend_of_record\n",
        b"DA:3,1\n",
    ] {
        assert!(evaluate(data, "/owned/snapshot", &[file()], 90., 80.).is_err());
    }
}
