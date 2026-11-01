//! A deliberately bounded JSON Schema profile for local tool calls.
//!
//! Compilation and grammar generation allocate; validation and fenced-JSON
//! repair borrow input and do not. Unsupported keywords fail at registration.
pub mod cli;
mod compile;
mod grammar;
mod json;
mod number;
mod registry;
mod validate;

pub use registry::{ToolRegistry, ValidatedCall};
use serde_json::Value;
use std::fmt;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_DEPTH: usize = 32;

/// Parse a bounded document, rejecting duplicate keys at every nesting level.
/// This registration/storage path allocates; tool argument validation does not.
pub fn parse_document(text: &str) -> anyhow::Result<Value> {
    compile::document(text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub offset: usize,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.code, self.offset)
    }
}

impl std::error::Error for ValidationError {}

type Result<T> = std::result::Result<T, ValidationError>;

#[derive(Debug)]
pub struct CompiledSchema {
    root: Node,
}

#[derive(Debug)]
struct Node {
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Object {
        properties: Vec<Property>,
        required: u128,
    },
    Array {
        items: Box<Node>,
        min: usize,
        max: usize,
    },
    String {
        min: usize,
        max: usize,
        choices: Option<Vec<String>>,
    },
    Number {
        integer: bool,
        min: Option<String>,
        max: Option<String>,
    },
    Boolean,
    Null,
}

#[derive(Debug)]
struct Property {
    name: String,
    node: Node,
}

/// The slice is usable only after the same validator accepts it, repaired or not.
#[derive(Debug)]
pub struct ValidatedInput<'a> {
    pub json: &'a str,
    pub repaired: bool,
}

impl CompiledSchema {
    /// Compile a trusted schema value. Use `from_json` for untrusted schema text
    /// so duplicate keys are rejected before serde can overwrite them.
    pub fn compile(schema: &Value) -> anyhow::Result<Self> {
        Ok(Self {
            root: compile::compile(schema)?,
        })
    }

    pub fn from_json(schema: &str) -> anyhow::Result<Self> {
        Self::compile(&compile::document(schema)?)
    }

    /// Validate one complete JSON value, including trailing-input rejection.
    pub fn validate(&self, input: &str) -> Result<()> {
        let mut parser = json::Parser::new(input)?;
        validate::node(&self.root, &mut parser, 0)?;
        parser.finish()
    }

    /// One conservative repair attempt: unwrap a sole Markdown JSON fence.
    /// No coercion, key removal, default insertion or punctuation repair occurs.
    pub fn validate_with_repair<'a>(
        &self,
        input: &'a str,
        repair: bool,
    ) -> Result<ValidatedInput<'a>> {
        match self.validate(input) {
            Ok(()) => Ok(ValidatedInput {
                json: input,
                repaired: false,
            }),
            Err(original) if repair && input.len() <= MAX_INPUT_BYTES => {
                let candidate = unfence(input).ok_or(original)?;
                self.validate(candidate)
                    .map_err(|e| shifted(e, input, candidate))?;
                Ok(ValidatedInput {
                    json: candidate,
                    repaired: true,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Structural GBNF. Numeric bounds remain enforced by `validate`.
    pub fn grammar(&self) -> String {
        grammar::generate(&self.root)
    }
}

fn unfence(input: &str) -> Option<&str> {
    let trimmed = input.trim_matches([' ', '\t', '\n', '\r']);
    let body = trimmed
        .strip_prefix("```json\n")
        .or_else(|| trimmed.strip_prefix("```\n"))?;
    body.strip_suffix("\n```")
}

fn shifted(mut error: ValidationError, original: &str, slice: &str) -> ValidationError {
    error.offset += slice.as_ptr() as usize - original.as_ptr() as usize;
    error
}
