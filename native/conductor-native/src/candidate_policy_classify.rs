//! Pure path classification for candidate changes.

use serde_json::{json, Value};
use std::collections::BTreeSet;

fn class_match_char(class: &[char], ch: char) -> bool {
    let (negative, chars) = if class.first() == Some(&'!') {
        (true, &class[1..])
    } else {
        (false, class)
    };
    let mut hit = false;
    let mut index = 0;
    while index < chars.len() {
        if index + 2 < chars.len() && chars[index + 1] == '-' {
            hit |= chars[index] <= ch && ch <= chars[index + 2];
            index += 3;
        } else {
            hit |= chars[index] == ch;
            index += 1;
        }
    }
    hit != negative
}

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

pub(crate) fn glob_match(path: &str, pattern: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    let mut current = vec![false; s.len() + 1];
    current[0] = true;
    let mut index = 0;
    while index < p.len() {
        let mut next = vec![false; s.len() + 1];
        if p[index] == '*' {
            while p.get(index) == Some(&'*') {
                index += 1;
            }
            next[0] = current[0];
            for offset in 1..=s.len() {
                next[offset] = current[offset] || next[offset - 1];
            }
        } else {
            let end = (p[index] == '[').then(|| class_end(&p, index)).flatten();
            for offset in 1..=s.len() {
                next[offset] = current[offset - 1]
                    && match end {
                        Some(close) => class_match_char(&p[index + 1..close], s[offset - 1]),
                        None => p[index] == '?' || p[index] == s[offset - 1],
                    };
            }
            index = end.map_or(index + 1, |close| close + 1);
        }
        current = next;
        if !current.iter().any(|matched| *matched) {
            return false;
        }
    }
    current[s.len()]
}

fn matches(path: &str, globs: &Value) -> bool {
    globs.as_array().is_some_and(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .any(|pattern| glob_match(path, pattern))
    })
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
    let path = change["path"]
        .as_str()
        .ok_or("candidate change requires path")?;
    let old_path = change["old_path"].as_str();
    let mut classes = BTreeSet::new();
    intrinsic(
        path,
        change["new_mode"].as_str().unwrap_or(""),
        &mut classes,
    );
    if let Some(old) = old_path {
        intrinsic(old, change["old_mode"].as_str().unwrap_or(""), &mut classes);
    }
    let mut paths = std::iter::once(path).chain(old_path);
    for (name, patterns) in globs["class_globs"]
        .as_object()
        .ok_or("candidate class globs must be a table")?
    {
        if paths.clone().any(|p| matches(p, patterns)) {
            classes.insert(name.clone());
        }
    }
    if paths.clone().any(|p| matches(p, &globs["generated_globs"])) {
        classes.insert("generated".into());
    }
    let risk = if paths.any(|p| matches(p, &globs["high_risk_globs"])) {
        "high"
    } else {
        "normal"
    };
    Ok(json!({"classes": classes.into_iter().collect::<Vec<_>>(), "risk": risk}))
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
}
