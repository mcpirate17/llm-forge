use super::*;
use serde_json::json;
#[test]
fn production_lines_are_compared_at_line_granularity() {
    let root = Path::new("/owned/snapshot");
    let source = Path::new("/live/repo");
    let tracked = ["src/model.py".into()].into_iter().collect();
    let baseline = json!({"files":{"src/model.py":{"executed_lines":[1,2],"excluded_lines":[]},"tests/test_x.py":{"executed_lines":[4],"excluded_lines":[]}}});
    let called = json!({"files":{"/owned/snapshot/src/model.py":{"executed_lines":[1,2,3],"excluded_lines":[]}}});
    let before = executed(&baseline, root, source, &tracked).unwrap();
    let after = executed(&called, root, source, &tracked).unwrap();
    assert_eq!(
        after.difference(&before).cloned().collect::<Vec<_>>(),
        [("src/model.py".into(), 3)]
    );
}
#[test]
fn hidden_untracked_live_and_malformed_evidence_fails_closed() {
    let root = Path::new("/owned/snapshot");
    let source = Path::new("/live/repo");
    let tracked = ["src/a.py".into()].into_iter().collect();
    for payload in [
        json!({"files":{"src/a.py":{"executed_lines":[1],"excluded_lines":[2]}}}),
        json!({"files":{"src/a.py":{"executed_lines":[1]}}}),
        json!({"files":{"src/new.py":{"executed_lines":[1],"excluded_lines":[]}}}),
        json!({"files":{"/live/repo/src/a.py":{"executed_lines":[1],"excluded_lines":[]}}}),
        json!({"files":{"../escape.py":{"executed_lines":[1],"excluded_lines":[]}}}),
    ] {
        assert!(executed(&payload, root, source, &tracked).is_err());
    }
    assert!(natives(&json!(["repo.so", "repo.so"])).is_err());
    assert_eq!(natives(&json!(["repo.so"])).unwrap(), ["repo.so"]);
}
