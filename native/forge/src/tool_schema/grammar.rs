//! Canonical property ordering avoids factorial object permutations. The
//! validator remains authoritative, especially for numeric range constraints.
use super::{Kind, Node, Property};
use std::fmt::Write;
#[path = "grammar_repeat.rs"]
mod repetition;

const PRELUDE: &str = r##"# forge tool schema: validate before dispatch; numeric bounds are runtime checks.
ws ::= [ \t]*
hex ::= [0-9a-fA-F]
scalar ::= [^"\\\x00-\x1f] | "\\" (["\\/bfnrt] | "u" ([0-9a-cA-Ce-fE-F] hex hex hex | [dD] [0-7] hex hex | [dD] [89aAbB] hex hex "\\u" [dD] [c-fC-F] hex hex))
integer ::= "-"? ("0" | [1-9] [0-9]*) ws
number ::= "-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [+-]? [0-9]{1,6})? ws
"##;

pub(super) fn generate(root: &Node) -> String {
    let mut generator = Generator {
        output: PRELUDE.to_owned(),
        next: 0,
    };
    generator.rule("root", "ws n-0");
    generator.node(root);
    generator.output
}

pub(super) fn calls<'a>(tools: impl Iterator<Item = (&'a str, &'a Node)>) -> String {
    let mut generator = Generator {
        output: PRELUDE.to_owned(),
        next: 0,
    };
    generator.rule("root", "ws tool-call");
    let alternatives: Vec<_> = tools
        .map(|(name, node)| {
            let arguments = generator.node(node);
            format!(
                "\"{{\" ws {} ws \":\" ws {} ws \",\" ws {} ws \":\" ws {arguments} \"}}\" ws",
                json_literal("name"),
                json_literal(name),
                json_literal("arguments")
            )
        })
        .collect();
    generator.rule("tool-call", &alternatives.join(" | "));
    generator.output
}

struct Generator {
    output: String,
    next: usize,
}

impl Generator {
    fn rule(&mut self, name: &str, body: &str) {
        writeln!(self.output, "{name} ::= {body}").expect("String writes cannot fail");
    }

    fn node(&mut self, node: &Node) -> String {
        let name = format!("n-{}", self.next);
        self.next += 1;
        let body = match &node.kind {
            Kind::Object {
                properties,
                required,
            } => self.object(&name, properties, *required),
            Kind::Array { items, min, max } => {
                let item = self.node(items);
                format!("\"[\" ws {} \"]\" ws", self.array(&item, *min, *max))
            }
            Kind::String { min, max, choices } => match choices {
                Some(values) => format!(
                    "({}) ws",
                    values
                        .iter()
                        .map(|s| json_literal(s))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                None => format!("\"\\\"\" {} \"\\\"\" ws", self.repeat("scalar", *min, *max)),
            },
            Kind::Number { integer: true, .. } => "integer".to_owned(),
            Kind::Number { .. } => "number".to_owned(),
            Kind::Boolean => "(\"true\" | \"false\") ws".to_owned(),
            Kind::Null => "\"null\" ws".to_owned(),
        };
        self.rule(&name, &body);
        name
    }

    fn object(&mut self, name: &str, properties: &[Property], required: u128) -> String {
        for (index, property) in properties.iter().enumerate() {
            let child = self.node(&property.node);
            for present in [false, true] {
                let state = usize::from(present);
                let prefix = if present { "\",\" ws " } else { "" };
                let next = index + 1;
                let emit = format!(
                    "{prefix}{} ws \":\" ws {child} {name}-p-{next}-1",
                    json_literal(&property.name)
                );
                let body = if required & (1u128 << index) == 0 {
                    format!("{emit} | {name}-p-{next}-{state}")
                } else {
                    emit
                };
                self.rule(&format!("{name}-p-{index}-{state}"), &body);
            }
        }
        for state in [0, 1] {
            self.rule(&format!("{name}-p-{}-{state}", properties.len()), "\"\"");
        }
        format!("\"{{\" ws {name}-p-0-0 \"}}\" ws")
    }

    fn repeat(&mut self, item: &str, min: usize, max: usize) -> String {
        if min <= 64 && (max <= 64 || max == usize::MAX) {
            return repeat(item, min, max);
        }
        let name = format!("repeat-{}", self.next);
        self.next += 1;
        let (rules, body) = repetition::bounded(&name, item, min, max);
        self.output.push_str(&rules);
        body
    }

    fn array(&mut self, item: &str, min: usize, max: usize) -> String {
        if max == 0 {
            return "\"\"".to_owned();
        }
        let tail_max = if max == usize::MAX { max } else { max - 1 };
        let tail = self.repeat(
            &format!("(\",\" ws {item})"),
            min.saturating_sub(1),
            tail_max,
        );
        let sequence = format!("{item} {tail}");
        if min == 0 {
            format!("({sequence})?")
        } else {
            sequence
        }
    }
}

fn json_literal(text: &str) -> String {
    // First encode the JSON string, then encode that text as a GBNF terminal.
    serde_json::to_string(&serde_json::to_string(text).expect("string JSON"))
        .expect("terminal JSON")
}

fn repeat(item: &str, min: usize, max: usize) -> String {
    match (min, max) {
        (0, 0) => "\"\"".to_owned(),
        (1, 1) => item.to_owned(),
        (0, usize::MAX) => format!("{item}*"),
        (1, usize::MAX) => format!("{item}+"),
        (_, usize::MAX) => format!("{item}{{{min},}}"),
        _ if min == max => format!("{item}{{{min}}}"),
        _ => format!("{item}{{{min},{max}}}"),
    }
}
