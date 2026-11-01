//! Shared native syntax inspection. Findings are lexical facts, not a type checker.
#[path = "syntax_projection.rs"]
mod projection;
#[path = "syntax_walk.rs"]
mod walk;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tree_sitter::{ParseOptions, Parser, Tree};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_AST_NODES: usize = 200_000;
const MAX_AST_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    C,
    Cpp,
    Javascript,
    Typescript,
    Tsx,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "c" | "h" => Some(Self::C),
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::Javascript),
            "ts" | "mts" | "cts" => Some(Self::Typescript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "rust" => Ok(Self::Rust),
            "python" => Ok(Self::Python),
            "c" => Ok(Self::C),
            "cpp" | "c++" => Ok(Self::Cpp),
            "javascript" => Ok(Self::Javascript),
            "typescript" => Ok(Self::Typescript),
            "tsx" => Ok(Self::Tsx),
            _ => bail!("unsupported source language: {name}"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Javascript => "javascript",
            Self::Typescript => "typescript",
            Self::Tsx => "tsx",
        }
    }

    pub fn scope_separator(self) -> &'static str {
        match self {
            Self::Rust | Self::C | Self::Cpp => "::",
            _ => ".",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Javascript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub qualified: String,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub byte_start: usize,
    pub byte_end: usize,
    pub signature: String,
    pub body: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub scope: String,
    pub role: String,
    pub line: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Dependency {
    pub target: String,
    pub kind: String,
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Inspection {
    pub language: Language,
    pub source_sha256: String,
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub dependencies: Vec<Dependency>,
    pub dynamic_calls: usize,
}

pub fn fingerprint(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

pub fn parse(source: &str, language: Language) -> Result<Tree> {
    ensure!(
        source.len() <= MAX_SOURCE_BYTES,
        "source exceeds {MAX_SOURCE_BYTES} bytes"
    );
    ensure!(!source.contains('\0'), "source contains NUL bytes");
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .context("load native grammar")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    let mut cancel = |_: &tree_sitter::ParseState| {
        if std::time::Instant::now() >= deadline {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let mut input = |offset, _| &source.as_bytes()[offset..];
    let tree = parser
        .parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut cancel)),
        )
        .context("syntax parsing exceeded 250 ms budget")?;
    if tree.root_node().has_error() {
        let mut node = tree.root_node();
        for _ in 0..MAX_AST_DEPTH {
            if node.is_error() || node.is_missing() {
                break;
            }
            let mut cursor = node.walk();
            let next = node
                .children(&mut cursor)
                .find(|child| child.has_error() || child.is_missing());
            let Some(next) = next else {
                break;
            };
            node = next;
        }
        bail!(
            "{} syntax error at line {}, byte column {} ({})",
            language.name(),
            node.start_position().row + 1,
            node.start_position().column + 1,
            node.kind()
        );
    }
    Ok(tree)
}

pub fn inspect(source: &str, language: Language) -> Result<Inspection> {
    let tree = parse(source, language)?;
    walk::inspect(source, language, &tree)
}

impl Inspection {
    /// Returns every lexical match; ambiguous definitions are never guessed.
    pub fn definitions(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.name == name || s.qualified == name)
            .collect()
    }

    pub fn references_to(&self, name: &str) -> Vec<&Reference> {
        self.references.iter().filter(|r| r.name == name).collect()
    }

    /// Context-only projection. The returned source must never be executed.
    pub fn fold(&self, source: &str) -> Result<String> {
        projection::fold(self, source)
    }

    pub fn graph_facts(&self) -> crate::graph_index::FileFacts {
        projection::facts(self)
    }
}
