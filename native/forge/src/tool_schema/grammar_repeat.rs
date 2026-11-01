//! Factored finite repetition: logarithmic rule depth instead of asking GBNF
//! consumers to expand a million literal optional repetitions during compilation.
use std::fmt::Write;

pub(super) fn bounded(name: &str, item: &str, min: usize, max: usize) -> (String, String) {
    let bound = if max == usize::MAX { min } else { max };
    debug_assert!(bound > 0);
    let highest = usize::BITS - 1 - bound.leading_zeros();
    let mut rules = String::new();
    writeln!(rules, "{name}-p-0 ::= {item}").expect("string write");
    for bit in 1..=highest {
        writeln!(
            rules,
            "{name}-p-{bit} ::= {name}-p-{} {name}-p-{}",
            bit - 1,
            bit - 1
        )
        .expect("string write");
    }
    let exact: Vec<_> = (0..=highest)
        .rev()
        .filter(|bit| min & (1usize << bit) != 0)
        .map(|bit| format!("{name}-p-{bit}"))
        .collect();
    let suffix = if max == usize::MAX {
        format!("{item}*")
    } else {
        at_most(name, max - min)
    };
    (rules, format!("{} {suffix}", exact.join(" ")))
}

fn at_most(name: &str, count: usize) -> String {
    if count == 0 {
        return "\"\"".into();
    }
    let bit = usize::BITS - 1 - count.leading_zeros();
    let power = 1usize << bit;
    let smaller = (0..bit)
        .rev()
        .map(|i| format!("{name}-p-{i}?"))
        .collect::<Vec<_>>()
        .join(" ");
    let smaller = if smaller.is_empty() {
        "\"\"".into()
    } else {
        smaller
    };
    format!(
        "({name}-p-{bit} {} | {smaller})",
        at_most(name, count - power)
    )
}
