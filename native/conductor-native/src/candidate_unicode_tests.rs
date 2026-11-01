use super::*;
#[test]
fn full_casefold_uses_unicode_15_default_not_simple_or_turkic() {
    for (input, expected) in [
        ("", ""),
        (
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ/0123_-.\n",
            "abcdefghijklmnopqrstuvwxyz/0123_-.\n",
        ),
        ("ASCII/Σ/ZX9", "ascii/σ/zx9"),
        ("Straße/STRASSE", "strasse/strasse"),
        ("Σςσ", "σσσ"),
        ("Iİıi", "ii\u{307}ıi"),
        ("ﬃ/ﬀ", "ffi/ff"),
        ("Ꭰꭰ", "ᎠᎠ"),
        ("\u{0390}", "ι\u{308}\u{301}"),
        ("\u{1c89}", "\u{1c89}"),
        ("é/e\u{301}", "é/e\u{301}"),
    ] {
        assert_eq!(casefold(input).unwrap(), expected, "{input:?}");
    }
}
#[test]
fn repr_quotes_controls_separators_and_non_ascii_like_python_312() {
    for (input, expected) in [
        ("plain café 🦀", "'plain café 🦀'"),
        ("a'b", "\"a'b\""),
        ("a\"b", "'a\"b'"),
        ("a'b\"c", "'a\\'b\"c'"),
        (
            "\\\n\r\t\0\u{b}\u{7f}\u{85}",
            "'\\\\\\n\\r\\t\\x00\\x0b\\x7f\\x85'",
        ),
        (
            " \u{a0}\u{2028}\u{2029}\u{200d}",
            "' \\xa0\\u2028\\u2029\\u200d'",
        ),
        ("\u{e000}\u{f0000}\u{0378}", "'\\ue000\\U000f0000\\u0378'"),
        ("\u{11f02}", "'\u{11f02}'"),
        ("\u{1c89}", "'\\u1c89'"),
    ] {
        assert_eq!(python_repr(input).unwrap(), expected, "{input:?}");
    }
}
#[test]
fn frozen_version_and_path_byte_limit_are_explicit() {
    assert_eq!(unicode_general_category::UNICODE_VERSION, UNICODE_VERSION);
    assert_eq!(table::CASE_FOLD.len(), 1530);
    assert!(table::CASE_FOLD
        .windows(2)
        .all(|pair| pair[0].0 < pair[1].0));
    assert!(casefold(&"x".repeat(4096)).is_ok());
    assert!(python_repr(&"x".repeat(4096)).is_ok());
    assert!(casefold(&"x".repeat(4097)).is_err());
    assert!(python_repr(&"é".repeat(2049)).is_err());
}
