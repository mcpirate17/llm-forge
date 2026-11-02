use super::*;
fn files(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("pkg/file.py".into(), text.into())])
}
fn patch(header: &str, body: &str) -> String {
    format!("diff --git a/pkg/file.py b/pkg/file.py\n--- a/pkg/file.py\n+++ b/pkg/file.py\n{header}\n{body}\n")
}
#[test]
fn exact_and_shifted_anchors_are_checked_without_modifying_input() {
    let inputs = files("prefix\nbefore\nold\nafter\n");
    let diff = patch("@@ -2,3 +2,3 @@", " before\n-old\n+new\n after");
    check(&diff, &inputs).unwrap();
    assert_eq!(inputs["pkg/file.py"], "prefix\nbefore\nold\nafter\n");
}
#[test]
fn context_can_relax_only_after_full_anchor_is_absent() {
    let inputs = files("other-before\nold\nother-after\n");
    let diff = patch("@@ -1,3 +1,3 @@", " before\n-old\n+new\n after");
    check(&diff, &inputs).unwrap();
}
#[test]
fn repeated_anchors_use_unique_nearest_location_and_reject_a_tie() {
    let inputs = files("old\nx\nold\n");
    check(&patch("@@ -1,1 +1,1 @@", "-old\n+new"), &inputs).unwrap();
    assert!(check(&patch("@@ -2,1 +2,1 @@", "-old\n+new"), &inputs).is_err());
}
#[test]
fn insertion_without_anchor_and_missing_or_unsafe_files_refuse() {
    assert!(check(&patch("@@ -1,0 +1,1 @@", "+new"), &files("a\n")).is_err());
    assert!(check(&patch("@@ -1 +1 @@", "-old\n+new"), &files("changed\n")).is_err());
    let escape = "--- a/../outside\n+++ b/../outside\n@@ -1 +1 @@\n-old\n+new\n";
    assert!(check(escape, &files("old\n")).is_err());
    assert!(check("--- /dev/null\n+++ b/new\n@@ -0 +1 @@\n+new\n", &files("")).is_err());
}
#[test]
fn hunks_apply_sequentially_in_memory_and_crlf_is_normalized() {
    let diff = "--- a/pkg/file.py\r\n+++ b/pkg/file.py\r\n@@ -1 +1 @@\r\n-old\r\n+middle\r\n@@ -1 +1 @@\r\n-middle\r\n+final\r\n";
    check(diff, &files("old\r\n")).unwrap();
}
