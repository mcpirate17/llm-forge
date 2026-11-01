use super::*;
fn hook(text: &str) -> Result<Vec<String>, String> {
    split(text, Dialect::HookShlex, None)
}
fn clang(text: &str) -> Result<Vec<String>, String> {
    split(
        text,
        Dialect::ClangGnu22,
        Some(Limits {
            input_bytes: 1024 * 1024,
            arguments: 4096,
            token_bytes: 64 * 1024,
            decoded_bytes: 1024 * 1024,
        }),
    )
}
#[test]
fn existing_hook_shlex_values_and_errors_are_unchanged() {
    let cases: &[(&str, &[&str])] = &[
        ("", &[]),
        (" \t\n\r", &[]),
        ("'' \"\"", &["", ""]),
        ("'a b' c", &["a b", "c"]),
        ("\"a b\"c", &["a bc"]),
        (r"a\ b", &["a b"]),
        (r#""a\"b""#, &["a\"b"]),
        (r#""a\qb" "a\\b""#, &[r"a\qb", r"a\b"]),
        (
            r"clang #hash a.c&&echo '*.c' '$HOME'",
            &["clang", "#hash", "a.c&&echo", "*.c", "$HOME"],
        ),
        ("a\\\nb", &["a\nb"]),
        ("\"a\\\nb\"", &["a\\\nb"]),
        ("雪\tß", &["雪", "ß"]),
    ];
    for (text, expected) in cases {
        assert_eq!(hook(text).unwrap(), *expected, "{text:?}");
    }
    for text in ["'open", "\"open", "\"trailing\\"] {
        assert_eq!(hook(text).unwrap_err(), "No closing quotation");
    }
    assert_eq!(hook("\\").unwrap_err(), "No escaped character");
}
#[test]
fn llvm22_compilation_quoting_has_explicit_literal_semantics() {
    // Literal expected argv from LLVM22 CommandLineArgumentParser, not shlex.
    let cases: &[(&str, &[&str])] = &[
        (
            "clang 'a b.c' \"\" ab\"cd\"",
            &["clang", "a b.c", "", "abcd"],
        ),
        (
            r#"clang "a\qb" "a\$b" "a\\b" "a\"b""#,
            &["clang", "aqb", "a$b", r"a\b", "a\"b"],
        ),
        (r"clang 'a\qb' a\ b", &["clang", r"a\qb", "a b"]),
        ("clang a\tb", &["clang", "a\tb"]),
        ("clang a\\\nb", &["clang", "a\nb"]),
        ("clang \"a\\\nb\"", &["clang", "a\nb"]),
        (
            r"clang -DNAME=#tag a.c&&echo $(id) '*.c'",
            &["clang", "-DNAME=#tag", "a.c&&echo", "$(id)", "*.c"],
        ),
        ("clang 雪\\ 山.c", &["clang", "雪 山.c"]),
    ];
    for (text, expected) in cases {
        assert_eq!(clang(text).unwrap(), *expected, "{text:?}");
    }
    for text in ["'open", "\"open", "\\", "\"trailing\\"] {
        assert!(
            clang(text).is_err(),
            "strict malformed-input refusal: {text:?}"
        );
    }
}
#[test]
fn emitted_empty_tokens_and_multibyte_tokens_obey_limits() {
    let limits = Limits {
        input_bytes: 64,
        arguments: 2,
        token_bytes: 4,
        decoded_bytes: 5,
    };
    let run = |text| split(text, Dialect::ClangGnu22, Some(limits));
    assert_eq!(run("雪 a").unwrap(), ["雪", "a"]);
    assert_eq!(run("'' ''").unwrap(), ["", ""]);
    assert!(run("'' '' ''").unwrap_err().contains("count"));
    assert!(run("雪山").unwrap_err().contains("token"));
    assert!(run("aaaa bb").unwrap_err().contains("decoded"));
    assert!(run(&" ".repeat(65)).unwrap_err().contains("input"));
}
