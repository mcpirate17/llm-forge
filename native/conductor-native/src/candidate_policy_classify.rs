//! Pure path classification for candidate changes.

use serde_json::{json, Value};
use std::collections::BTreeSet;

fn class_end(pattern: &[char], start: usize) -> Option<usize> {
    let mut end = start + 1;
    if pattern.get(end) == Some(&'!') {
        end += 1;
    }
    // As in fnmatchcase, the first ']' is a class member, not its terminator.
    if pattern.get(end) == Some(&']') {
        end += 1;
    }
    (end..pattern.len()).find(|&index| pattern[index] == ']')
}

#[derive(Clone)]
enum Token {
    Star,
    Any,
    Literal(char),
    Class {
        negative: bool,
        ranges: Vec<(char, char)>,
    },
}
struct Pattern {
    tokens: Vec<Token>,
}
impl Pattern {
    fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.chars().collect();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < chars.len() {
            match chars[index] {
                '*' => {
                    tokens.push(Token::Star);
                    while chars.get(index) == Some(&'*') {
                        index += 1;
                    }
                }
                '?' => {
                    tokens.push(Token::Any);
                    index += 1;
                }
                '[' => match class_end(&chars, index) {
                    Some(end) => {
                        tokens.push(compile_class(&chars[index + 1..end]));
                        index = end + 1;
                    }
                    None => {
                        tokens.push(Token::Literal('['));
                        index += 1;
                    }
                },
                ch => {
                    tokens.push(Token::Literal(ch));
                    index += 1;
                }
            }
        }
        Self { tokens }
    }
}
fn compile_class(class: &[char]) -> Token {
    let (negative, chars) = if class.first() == Some(&'!') {
        (true, &class[1..])
    } else {
        (false, class)
    };
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if index + 2 < chars.len() && chars[index + 1] == '-' {
            ranges.push((chars[index], chars[index + 2]));
            index += 3;
        } else {
            ranges.push((chars[index], chars[index]));
            index += 1;
        }
    }
    Token::Class { negative, ranges }
}
#[derive(Default)]
struct Workspace {
    current: Vec<bool>,
    next: Vec<bool>,
}
impl Workspace {
    fn matches(&mut self, path: &[char], pattern: &Pattern) -> bool {
        let len = path.len() + 1;
        self.current.resize(len, false);
        self.next.resize(len, false);
        self.current.fill(false);
        self.current[0] = true;
        for token in &pattern.tokens {
            self.next.fill(false);
            if matches!(token, Token::Star) {
                self.next[0] = self.current[0];
                for offset in 1..len {
                    self.next[offset] = self.current[offset] || self.next[offset - 1];
                }
            } else {
                for offset in 1..len {
                    self.next[offset] =
                        self.current[offset - 1] && token_matches(token, path[offset - 1]);
                }
            }
            std::mem::swap(&mut self.current, &mut self.next);
            if !self.current.iter().any(|matched| *matched) {
                return false;
            }
        }
        self.current[path.len()]
    }
}
fn token_matches(token: &Token, ch: char) -> bool {
    match token {
        Token::Any => true,
        Token::Literal(value) => *value == ch,
        Token::Class { negative, ranges } => {
            ranges.iter().any(|(start, end)| *start <= ch && ch <= *end) != *negative
        }
        Token::Star => unreachable!("star handled separately"),
    }
}
pub fn glob_match(path: &str, pattern: &str) -> bool {
    let path: Vec<char> = path.chars().collect();
    Workspace::default().matches(&path, &Pattern::new(pattern))
}
fn compile_patterns(value: &Value) -> Vec<Pattern> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(Pattern::new)
        .collect()
}
fn any_match(
    patterns: &[Pattern],
    new: &[char],
    old: Option<&[char]>,
    workspace: &mut Workspace,
) -> bool {
    // Preserve original path-first ordering; matches are pure so either order has same result.
    std::iter::once(new).chain(old).any(|path| {
        patterns
            .iter()
            .any(|pattern| workspace.matches(path, pattern))
    })
}

/// Compiled policy patterns and reusable work buffers for a batch of changes.
pub struct PreparedClassifier {
    classes: Vec<(String, Vec<Pattern>)>,
    generated: Vec<Pattern>,
    high_risk: Vec<Pattern>,
    new_path: Vec<char>,
    old_path: Vec<char>,
    workspace: Workspace,
}
impl PreparedClassifier {
    pub fn new(globs: &Value) -> Result<Self, String> {
        let classes = globs["class_globs"]
            .as_object()
            .ok_or("candidate class globs must be a table")?
            .iter()
            .map(|(name, patterns)| (name.clone(), compile_patterns(patterns)))
            .collect();
        Ok(Self {
            classes,
            generated: compile_patterns(&globs["generated_globs"]),
            high_risk: compile_patterns(&globs["high_risk_globs"]),
            new_path: Vec::new(),
            old_path: Vec::new(),
            workspace: Workspace::default(),
        })
    }
    pub fn classify(&mut self, change: &Value) -> Result<Value, String> {
        let path = change["path"]
            .as_str()
            .ok_or("candidate change requires path")?;
        let old = change["old_path"].as_str();
        let mut classes = BTreeSet::new();
        intrinsic(
            path,
            change["new_mode"].as_str().unwrap_or(""),
            &mut classes,
        );
        if let Some(old) = old {
            intrinsic(old, change["old_mode"].as_str().unwrap_or(""), &mut classes);
        }
        self.new_path.clear();
        self.new_path.extend(path.chars());
        self.old_path.clear();
        if let Some(path) = old {
            self.old_path.extend(path.chars());
        }
        let old_chars = old.map(|_| self.old_path.as_slice());
        for (name, patterns) in &self.classes {
            if any_match(patterns, &self.new_path, old_chars, &mut self.workspace) {
                classes.insert(name.clone());
            }
        }
        if any_match(
            &self.generated,
            &self.new_path,
            old_chars,
            &mut self.workspace,
        ) {
            classes.insert("generated".into());
        }
        let risk = if any_match(
            &self.high_risk,
            &self.new_path,
            old_chars,
            &mut self.workspace,
        ) {
            "high"
        } else {
            "normal"
        };
        Ok(json!({"classes": classes.into_iter().collect::<Vec<_>>(), "risk": risk}))
    }
}

fn is_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| part == "test")
        || name.starts_with("test_")
        || [
            "_test.py",
            "_test.c",
            "_test.cc",
            "_test.cpp",
            "_test.cxx",
            ".test.js",
            ".test.jsx",
            ".test.ts",
            ".test.tsx",
            ".spec.js",
            ".spec.jsx",
            ".spec.ts",
            ".spec.tsx",
            "Test.java",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn intrinsic(path: &str, mode: &str, classes: &mut BTreeSet<String>) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let suffix = name
        .rsplit_once('.')
        .map(|(_, extension)| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_default();
    let mut add = |items: &[&str]| {
        classes.extend(items.iter().map(|s| (*s).to_owned()));
    };
    match suffix.as_str() {
        ".py" | ".pyi" => add(&["python", "source"]),
        ".rs" => add(&["native", "source", "rust"]),
        ".c" | ".cc" | ".cpp" | ".cxx" | ".h" | ".hpp" => {
            add(&["native", "source", "cfamily", "cfamily_host"])
        }
        ".cu" | ".cuh" => add(&["native", "source", "cfamily", "cuda"]),
        ".js" | ".jsx" | ".ts" | ".tsx" | ".css" => add(&["source", "web"]),
        ".sh" | ".bash" => add(&["source", "shell"]),
        ".md" | ".rst" | ".txt" => add(&["docs"]),
        ".ipynb" => add(&["notebook"]),
        ".toml" => add(&["config", "toml"]),
        ".yaml" | ".yml" | ".json" | ".ini" | ".cfg" => add(&["config"]),
        _ => {}
    }
    if [".so", ".dll", ".dylib", ".a", ".o", ".pt", ".pth", ".bin"].contains(&suffix.as_str()) {
        add(&["binary"]);
    }
    if [
        "pyproject.toml",
        "uv.lock",
        "requirements.txt",
        "requirements-dev.txt",
    ]
    .contains(&name)
    {
        add(&["dependency", "python_dependency"]);
    }
    if ["package.json", "package-lock.json", "npm-shrinkwrap.json"].contains(&name) {
        add(&["dependency", "node_dependency"]);
    }
    if ["Cargo.toml", "Cargo.lock"].contains(&name) {
        add(&["dependency", "rust_dependency"]);
    }
    if is_test(path) {
        add(&["test"]);
    }
    if mode == "120000" {
        add(&["symlink"]);
    }
}

pub fn classify(change: &Value, globs: &Value) -> Result<Value, String> {
    // Retain the old error precedence when both inputs are malformed.
    change["path"]
        .as_str()
        .ok_or("candidate change requires path")?;
    PreparedClassifier::new(globs)?.classify(change)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fnmatch_star_crosses_slashes_and_classes_are_case_sensitive() {
        assert!(glob_match("src/a/b.py", "src/*.py"));
        assert!(glob_match("ab7.py", "ab[0-9].py"));
        assert!(!glob_match("AB7.py", "ab[0-9].py"));
        assert!(glob_match("abx.py", "ab[!0-9].py"));
        assert!(glob_match("]", "[]]"));
        assert!(!glob_match("]", "[!]]"));
        assert!(glob_match("x", "[!]]"));
        assert!(glob_match("a".repeat(20_000).as_str(), "a*"));
        assert!(glob_match("x", &"*".repeat(10_000)));
    }
    #[test]
    fn glob_classes_handle_unicode_reversed_ranges_and_malformed_brackets() {
        for (path, pattern, expected) in [
            ("猫", "[猫-龍]", true),
            ("é", "[a-z]", false),
            ("é", "[!a-z]", true),
            ("a", "[z-a]", false),
            ("a", "[!z-a]", true),
            ("[abc", "[abc", true),
            ("[]", "[]", true),
            ("-", "[-a]", true),
            ("]", "[]-]", true),
            ("🦀/文.py", "?/?.py", true),
        ] {
            assert_eq!(glob_match(path, pattern), expected, "{path:?} {pattern:?}");
        }
    }

    #[test]
    fn prepared_classifier_reuses_buffers_without_leaking_prior_matches() {
        let globs = json!({"class_globs":{"watch":["secret/*", 17]},
            "generated_globs":["safe/*"], "high_risk_globs":["secret/*"]});
        let mut classifier = PreparedClassifier::new(&globs).unwrap();
        let renamed = json!({"path":"safe/new.md","new_mode":"100644",
            "old_path":"secret/test_é.py","old_mode":"120000"});
        assert_eq!(
            classifier.classify(&renamed).unwrap(),
            json!({
            "classes":["docs","generated","python","source","symlink","test","watch"],"risk":"high"})
        );
        assert_eq!(
            classifier.classify(&json!({"path":"x"})).unwrap(),
            json!({"classes":[],"risk":"normal"})
        );
        assert_eq!(
            classifier
                .classify(&json!({"path":"Cargo.toml","status":"D"}))
                .unwrap(),
            json!({"classes":["config","dependency","rust_dependency","toml"],"risk":"normal"})
        );
        assert_eq!(
            classifier.classify(&renamed).unwrap(),
            classify(&renamed, &globs).unwrap()
        );
    }
}
