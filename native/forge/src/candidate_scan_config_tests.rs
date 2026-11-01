use super::*;

fn parse(paths: &str) -> Result<Vec<String>> {
    hot_paths(Some(
        format!("[tool.forge.hot_paths]\npaths = {paths}\n").as_bytes(),
    ))
}

#[test]
fn absent_table_is_explicit_empty_scope() {
    assert_eq!(hot_paths(None).unwrap(), Vec::<String>::new());
    assert_eq!(
        hot_paths(Some(b"[tool.forge]\nname = 'test'\n")).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn documented_paths_are_normalized_and_ordered() {
    assert_eq!(
        parse("['src/train/', 'research/runtime', 'space name/δ']").unwrap(),
        vec!["src/train", "research/runtime", "space name/δ"]
    );
    assert!(parse("['src/train', 'src/train/']").is_err());
}

#[test]
fn unsafe_ambiguous_and_wrong_types_refuse() {
    for path in [
        "/abs",
        ".",
        "./dir",
        "dir/../other",
        "dir//other",
        "dir//",
        "dir\\other",
    ] {
        assert!(normalize(path).is_err(), "{path}");
    }
    assert!(normalize("dir/\nother").is_err());
    assert!(parse("[]").is_err());
    assert!(parse("['okay', 3]").is_err());
    assert!(hot_paths(Some(b"[tool.forge.hot_paths]\npaths=['src']\nother=true\n")).is_err());
    assert!(hot_paths(Some(b"[tool.forge.hot_paths]\npaths='src'\n")).is_err());
    assert!(hot_paths(Some(
        b"[tool.forge.hot_paths]\npaths=['src']\npaths=['foo']\n"
    ))
    .is_err());
    assert!(hot_paths(Some(b"\xff")).is_err());
}

#[test]
fn bounds_refuse_without_truncating() {
    assert!(hot_paths(Some(&vec![b' '; MAX_DOCUMENT + 1])).is_err());
    assert!(normalize(&"x".repeat(MAX_PATH + 1)).is_err());
    let paths = (0..65)
        .map(|i| format!("'p{i}'"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(parse(&format!("[{paths}]")).is_err());
}
