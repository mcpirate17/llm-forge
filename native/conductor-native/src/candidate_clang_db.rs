//! Pure bounded compilation-database admission; no path rebasing or execution.
//! Command quoting: LLVM llvmorg-22.1.0 JSONCompilationDatabase.cpp lines44–141.
//! Strict differences: controls and unfinished quoting/escapes are rejected.
use crate::command_lex::{self, Dialect};
use serde::{
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use std::fmt;
const MAX_INPUT: usize = 32 * 1024 * 1024;
const MAX_DECODED: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_ARGUMENTS: usize = 4096;
const MAX_TOKEN: usize = 64 * 1024;
const MAX_COMMAND: usize = 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_TOTAL_ARGUMENTS: usize = 1_000_000;
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Compilation {
    pub directory: String,
    pub file: String,
    pub arguments: Vec<String>,
    pub output: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Database {
    /// Every configuration in original record order, including duplicates.
    pub entries: Vec<Compilation>,
}
#[derive(Clone, Copy)]
struct Limits {
    input: usize,
    decoded: usize,
    records: usize,
    arguments: usize,
    token: usize,
    command: usize,
    path: usize,
    total_arguments: usize,
}
const LIMITS: Limits = Limits {
    input: MAX_INPUT,
    decoded: MAX_DECODED,
    records: MAX_RECORDS,
    arguments: MAX_ARGUMENTS,
    token: MAX_TOKEN,
    command: MAX_COMMAND,
    path: MAX_PATH,
    total_arguments: MAX_TOTAL_ARGUMENTS,
};
struct Budget {
    limits: Limits,
    decoded: usize,
    arguments: usize,
}
impl Budget {
    fn charge<E: de::Error>(&mut self, bytes: usize) -> Result<(), E> {
        self.decoded = self
            .decoded
            .checked_add(bytes)
            .filter(|total| *total <= self.limits.decoded)
            .ok_or_else(|| E::custom("compilation database exceeds decoded byte limit"))?;
        Ok(())
    }
    fn argument<E: de::Error>(&mut self) -> Result<(), E> {
        self.arguments = self
            .arguments
            .checked_add(1)
            .filter(|count| *count <= self.limits.total_arguments)
            .ok_or_else(|| E::custom("compilation database exceeds total argument count limit"))?;
        Ok(())
    }
}
/// Unknown fields, duplicate keys (including escaped aliases), null values,
/// controls and malformed records fail closed. Empty databases are valid.
/// Decoded accounting includes both supplied representations and derived argv.
pub fn parse_database(raw: &[u8]) -> Result<Database, String> {
    parse_with_limits(raw, LIMITS)
}
fn parse_with_limits(raw: &[u8], limits: Limits) -> Result<Database, String> {
    if raw.len() > limits.input {
        return Err("compilation database exceeds input byte limit".into());
    }
    std::str::from_utf8(raw).map_err(|_| "compilation database is not UTF8")?;
    let mut budget = Budget {
        limits,
        decoded: 0,
        arguments: 0,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let entries = DatabaseSeed(&mut budget)
        .deserialize(&mut deserializer)
        .map_err(parse_error)?;
    deserializer.end().map_err(parse_error)?;
    Ok(Database { entries })
}
fn parse_error(error: serde_json::Error) -> String {
    let message = error.to_string();
    let admitted = [
        "compilation database exceeds record count limit",
        "compilation database exceeds argument count limit",
        "compilation database exceeds total argument count limit",
        "compilation database exceeds decoded byte limit",
        "compilation command exceeds input byte limit",
        "compilation command exceeds argument count limit",
        "compilation command exceeds decoded byte limit",
        "compilation argument exceeds token byte limit",
        "compilation string exceeds byte limit",
        "empty or control-containing compilation string",
        "duplicate compilation field",
        "unknown compilation field",
        "missing compilation directory",
        "missing compilation file",
        "missing compilation arguments or command",
        "arguments and command disagree",
        "compilation requires a nonempty executable argument",
        "No closing quotation",
        "No escaped character",
    ];
    let reason = admitted
        .iter()
        .find(|prefix| message.starts_with(**prefix))
        .map_or("invalid JSON compilation shape or encoding", |prefix| {
            *prefix
        });
    format!(
        "invalid compilation database: {reason} at line {} column {}",
        error.line(),
        error.column()
    )
}
struct DatabaseSeed<'a>(&'a mut Budget);
impl<'de> DeserializeSeed<'de> for DatabaseSeed<'_> {
    type Value = Vec<Compilation>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for DatabaseSeed<'_> {
    type Value = Vec<Compilation>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a compilation array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut entries = Vec::new();
        loop {
            // At capacity inspect only whether another value exists; never decode it.
            if entries.len() == self.0.limits.records {
                seq.next_element_seed(RejectExtra("record count"))?;
                return Ok(entries);
            }
            match seq.next_element_seed(CompilationSeed(self.0))? {
                Some(entry) => entries.push(entry),
                None => return Ok(entries),
            }
        }
    }
}
struct RejectExtra(&'static str);
impl<'de> DeserializeSeed<'de> for RejectExtra {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
        Err(de::Error::custom(format!(
            "compilation database exceeds {} limit",
            self.0
        )))
    }
}
#[derive(Clone, Copy)]
enum Field {
    Directory,
    File,
    Arguments,
    Command,
    Output,
}
impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = Field;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a compilation field")
            }
            fn visit_str<E: de::Error>(self, key: &str) -> Result<Field, E> {
                match key {
                    "directory" => Ok(Field::Directory),
                    "file" => Ok(Field::File),
                    "arguments" => Ok(Field::Arguments),
                    "command" => Ok(Field::Command),
                    "output" => Ok(Field::Output),
                    _ => Err(E::custom("unknown compilation field")),
                }
            }
        }
        d.deserialize_identifier(FieldVisitor)
    }
}
struct CompilationSeed<'a>(&'a mut Budget);
impl<'de> DeserializeSeed<'de> for CompilationSeed<'_> {
    type Value = Compilation;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Compilation, D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for CompilationSeed<'_> {
    type Value = Compilation;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a compilation object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Compilation, A::Error> {
        let mut seen = 0u8;
        let (mut directory, mut file, mut arguments, mut command, mut output) =
            (None, None, None, None, None);
        while let Some(key) = map.next_key::<Field>()? {
            let bit = 1 << key as u8;
            if seen & bit != 0 {
                return Err(de::Error::custom("duplicate compilation field"));
            }
            seen |= bit;
            match key {
                Field::Directory => {
                    directory = Some(map.next_value_seed(TextSeed::path(self.0, true))?)
                }
                Field::File => file = Some(map.next_value_seed(TextSeed::path(self.0, true))?),
                Field::Output => output = Some(map.next_value_seed(TextSeed::path(self.0, false))?),
                Field::Arguments => arguments = Some(map.next_value_seed(ArgumentsSeed(self.0))?),
                Field::Command => {
                    let max = self.0.limits.command;
                    command = Some(map.next_value_seed(TextSeed {
                        budget: self.0,
                        max,
                        nonempty: true,
                    })?);
                }
            }
        }
        let directory = directory
            .ok_or_else(|| <A::Error as de::Error>::custom("missing compilation directory"))?;
        let file =
            file.ok_or_else(|| <A::Error as de::Error>::custom("missing compilation file"))?;
        let arguments = resolve_arguments(self.0, arguments, command)?;
        Ok(Compilation {
            directory,
            file,
            arguments,
            output,
        })
    }
}
struct TextSeed<'a> {
    budget: &'a mut Budget,
    max: usize,
    nonempty: bool,
}
impl<'a> TextSeed<'a> {
    fn path(budget: &'a mut Budget, nonempty: bool) -> Self {
        Self {
            max: budget.limits.path,
            budget,
            nonempty,
        }
    }
}
impl<'de> DeserializeSeed<'de> for TextSeed<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_str(self)
    }
}
impl Visitor<'_> for TextSeed<'_> {
    type Value = String;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a bounded control-free string")
    }
    fn visit_str<E: de::Error>(self, text: &str) -> Result<String, E> {
        if text.len() > self.max {
            return Err(E::custom("compilation string exceeds byte limit"));
        }
        if (self.nonempty && text.is_empty()) || text.chars().any(char::is_control) {
            return Err(E::custom("empty or control-containing compilation string"));
        }
        self.budget.charge::<E>(text.len())?;
        Ok(text.to_owned())
    }
}
struct ArgumentsSeed<'a>(&'a mut Budget);
impl<'de> DeserializeSeed<'de> for ArgumentsSeed<'_> {
    type Value = Vec<String>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for ArgumentsSeed<'_> {
    type Value = Vec<String>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an argv array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut args = Vec::new();
        loop {
            if args.len() == self.0.limits.arguments {
                seq.next_element_seed(RejectExtra("argument count"))?;
                return Ok(args);
            }
            let max = self.0.limits.token;
            let seed = TextSeed {
                budget: self.0,
                max,
                nonempty: args.is_empty(),
            };
            match seq.next_element_seed(seed)? {
                Some(arg) => {
                    self.0.argument::<A::Error>()?;
                    args.push(arg);
                }
                None => return Ok(args),
            }
        }
    }
}
fn resolve_arguments<E: de::Error>(
    budget: &mut Budget,
    arguments: Option<Vec<String>>,
    command: Option<String>,
) -> Result<Vec<String>, E> {
    let decoded = command
        .map(|command| {
            let limits = command_lex::Limits {
                input_bytes: budget.limits.command,
                arguments: budget.limits.arguments.min(
                    budget
                        .limits
                        .total_arguments
                        .saturating_sub(budget.arguments),
                ),
                token_bytes: budget.limits.token,
                decoded_bytes: budget.limits.decoded.saturating_sub(budget.decoded),
            };
            let argv = command_lex::split(&command, Dialect::ClangGnu22, Some(limits))
                .map_err(E::custom)?;
            for arg in &argv {
                budget.argument::<E>()?;
                budget.charge::<E>(arg.len())?;
            }
            Ok::<_, E>(argv)
        })
        .transpose()?;
    if let (Some(args), Some(decoded)) = (&arguments, &decoded) {
        if args != decoded {
            return Err(E::custom("arguments and command disagree"));
        }
    }
    let result = arguments
        .or(decoded)
        .ok_or_else(|| E::custom("missing compilation arguments or command"))?;
    if result.first().is_none_or(String::is_empty) {
        return Err(E::custom(
            "compilation requires a nonempty executable argument",
        ));
    }
    Ok(result)
}
#[cfg(test)]
#[path = "candidate_clang_db_tests.rs"]
mod tests;
