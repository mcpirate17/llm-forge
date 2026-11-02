use super::*;

#[test]
fn universal_lines_preserve_original_utf8_byte_offsets() {
    let source = "é\rX\r\nz\n";
    let lines = Lines::new(source);
    assert_eq!(lines.text(source), vec!["é", "X", "z"]);
    assert_eq!((lines.number(3), lines.column(3)), (2, 0));
    assert_eq!((lines.number(6), lines.column(6)), (3, 0));
    assert_eq!((lines.number(8), lines.column(8)), (4, 0));
    assert!(Lines::new("").text("").is_empty());
}

#[test]
fn duplicate_function_spans_count_bare_cr_lines() {
    use crate::duplicate_fingerprints::{analyze_document, FingerprintPolicy};
    let source = format!("def f():\r{}    return 1\r", "    x=1\r".repeat(9));
    let document = Document::parse(&source).unwrap();
    let output = analyze_document("x.py".into(), &document, FingerprintPolicy::Candidate);
    assert_eq!(output.functions.len(), 1);
    assert_eq!(output.functions[0].lineno, 1);
    assert_eq!(output.functions[0].end_lineno, 11);
}

#[test]
fn captured_unicode_source_and_tokens_share_the_same_module() {
    let source = "from café import résumé\nvalue = f'{résumé}'\n";
    let document = Document::parse(source).unwrap();
    assert_eq!(document.text(), source);
    assert_eq!(document.module().body.len(), 2);
    assert!(!document.parsed().tokens().is_empty());
    assert!(document.nodes() > 4);
}

#[test]
fn malformed_input_is_distinct_from_exhausted_admission() {
    assert!(matches!(
        Document::parse("def broken(:\n"),
        Err(Error::Syntax { .. })
    ));
    assert!(matches!(
        Document::parse("x = '\0'"),
        Err(Error::Syntax { offset: 5 })
    ));
    assert!(matches!(
        Document::parse(&" ".repeat(MAX_BYTES + 1)),
        Err(Error::Limit(_))
    ));
}

#[test]
fn recursive_shapes_are_refused_before_the_recursive_parser() {
    for operator in [
        "+", "-", "*", "/", "//", "%", "**", "@", "<<", ">>", "&", "|", "^",
    ] {
        let source = format!("x = {}1\n", format!("1{operator}").repeat(4096));
        assert!(
            matches!(preflight(&source), Err(Error::Limit(_))),
            "{operator}"
        );
        assert!(
            matches!(Document::parse(&source), Err(Error::Limit(_))),
            "{operator}"
        );
    }
    for source in [
        format!("x = {}1{}\n", "(".repeat(4096), ")".repeat(4096)),
        format!("x = {}1\n", "-".repeat(4096)),
        format!("x = {}1\n", "1+".repeat(4096)),
        format!("x = value{}\n", ".field".repeat(4096)),
        format!("x = {}0{}\n", "1 if True else ".repeat(4096), " "),
        format!("x = {}1\n", "lambda: ".repeat(4096)),
    ] {
        assert!(matches!(preflight(&source), Err(Error::Limit(_))));
        assert!(matches!(Document::parse(&source), Err(Error::Limit(_))));
    }
}

#[test]
fn wide_literals_and_independent_statements_do_not_accumulate_recursion_budget() {
    let list = format!("values = [{}]\n", "1,".repeat(3000));
    assert!(Document::parse(&list).is_ok());
    let lines = "value = first + second\n".repeat(3000);
    assert!(Document::parse(&lines).is_ok());
    let text = "value = 'lambda: ((((( ... 1+1+1'\n# [ ( lambda:\n";
    assert!(Document::parse(text).is_ok());
}

#[test]
fn ast_shape_and_indentation_limits_are_enforced() {
    let source = format!("value = base{}\n", ".field".repeat(150));
    assert!(preflight(&source).is_ok());
    assert!(matches!(Document::parse(&source), Err(Error::Limit(_))));
    let nested = (0..70)
        .map(|depth| format!("{}if True:\n", " ".repeat(depth)))
        .collect::<String>()
        + &format!("{}pass\n", " ".repeat(70));
    assert!(matches!(preflight(&nested), Err(Error::Limit(_))));
    let flat = "x = 1\n".repeat(24000);
    assert!(matches!(Document::parse(&flat), Err(Error::Limit(_))));
}
