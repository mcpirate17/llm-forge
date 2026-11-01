use super::*;

#[test]
fn removes_exact_cfg_items_and_all_adjacent_attributes() {
    let input = "pub fn before() {}\n/// only tests\n#[allow(dead_code)]\n#[ cfg ( /*comment*/ test ) ]\n#[allow(unused)]\nmod tests { fn t() {} }\npub fn after() {}\n";
    assert_eq!(
        production_source(input).unwrap(),
        "pub fn before() {}\n\npub fn after() {}\n"
    );
    let input = "#[cfg(test)] use crate::abc;\n#[cfg(test)] static VALUE: u8 = 3;\nfn keep() {}\n";
    assert_eq!(production_source(input).unwrap(), "\n\nfn keep() {}\n");
}

#[test]
fn removes_nested_tests_without_damaging_production_item_or_doc() {
    let input = "mod outer {\n  fn live() {}\n  #[cfg(test)]\n  mod tests { fn inner() {} }\n  /// live doc\n  fn live2() {}\n}\n";
    assert_eq!(
        production_source(input).unwrap(),
        "mod outer {\n  fn live() {}\n  \n  /// live doc\n  fn live2() {}\n}\n"
    );
}

#[test]
fn strings_comments_and_nonexact_cfg_are_preserved() {
    let input = r###"// #[cfg(test)] mod fake {}
const A: &str = r#"#[cfg(test)] { /* } */ }"#;
const B: &[u8] = b"#[cfg(test)]";
const C: char = '}';
#[cfg(any(test, feature = "shipping"))]
fn maybe_shipping() {}
#[cfg(not(test))]
fn production() {}
"###;
    assert_eq!(production_source(input).unwrap(), input);
}

#[test]
fn duplicate_test_guards_and_parent_guard_remove_single_complete_item() {
    let input =
        "#[cfg(test)]\n#[cfg(test)]\nmod tests { #[cfg(test)] fn child() {} }\nfn keep() {}\n";
    assert_eq!(production_source(input).unwrap(), "\nfn keep() {}\n");
}

#[test]
fn malformed_oversized_nul_and_unsupported_inner_attributes_refuse() {
    assert!(production_source("fn broken( {\n").is_err());
    assert!(production_source("const S: &str = \"\0\";").is_err());
    assert!(production_source(&" ".repeat(syntax::MAX_SOURCE_BYTES + 1)).is_err());
    assert!(production_source("mod x { #![cfg(test)] fn test() {} }").is_err());
    let deep = format!(
        "fn f() {{ let x = {}1{}; }}",
        "{".repeat(140),
        "}".repeat(140)
    );
    assert!(production_source(&deep).is_err());
    let many = "const X: usize = 0;\n".repeat(20_000);
    assert!(many.len() < syntax::MAX_SOURCE_BYTES);
    assert!(production_source(&many).is_err());
}
