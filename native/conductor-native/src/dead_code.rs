//! Evidence-based adjudication of Vulture candidates across a repository.
//!
//! This deliberately preserves the scratch classifier's bare-name semantics:
//! `ALIVE` means a name was observed outside the candidate's file, not that a
//! resolved call graph proves the definition is reachable. All evidence is kept.

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub kind: String,
    pub name: String,
    pub conf: usize,
    pub lines: Option<String>,
    pub size: usize,
}

/// Relative repository path, one-based line, and evidence kind.
pub type Reference = (String, usize, String);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Row {
    #[serde(flatten)]
    pub finding: Finding,
    pub verdict: String,
    pub prod: Vec<Reference>,
    pub tests: Vec<Reference>,
    pub docs: Vec<Reference>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuditReport {
    pub schema_version: u32,
    /// False if any eligible input could not be read or parsed. Verdicts then
    /// describe only the observed subset and cannot establish missing references.
    pub complete: bool,
    pub counts: BTreeMap<String, usize>,
    pub findings: Vec<Row>,
    pub diagnostics: Vec<Diagnostic>,
    pub scanned_files: usize,
}

/// Parse the human-readable Vulture stream, including `--sort-by-size` output.
/// Unexpected lines fail rather than silently discarding detector findings.
pub fn parse_vulture_output(output: &str) -> Result<Vec<Finding>> {
    let pattern = Regex::new(concat!(
        r"^(?P<file>.+):(?P<line>\d+): unused ",
        r"(?P<kind>variable|attribute|function|method|class|import|property) ",
        r"'(?P<name>[^']+)' \((?P<conf>\d+)% confidence",
        r"(?:, (?P<lines>\d+) lines?)?\)$"
    ))?;
    output
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(number, line)| {
            let captures = pattern.captures(line.trim()).with_context(|| {
                format!("unrecognized Vulture output on line {}: {line}", number + 1)
            })?;
            let line = captures["line"].parse::<usize>()?;
            let conf = captures["conf"].parse::<usize>()?;
            let lines = captures
                .name("lines")
                .map(|value| value.as_str().to_owned());
            let size = lines.as_deref().unwrap_or("1").parse::<usize>()?;
            if line == 0 || conf > 100 || size == 0 {
                bail!(
                    "invalid Vulture location, confidence, or size on line {}",
                    number + 1
                );
            }
            Ok(Finding {
                file: captures["file"].to_owned(),
                line,
                kind: captures["kind"].to_owned(),
                name: captures["name"].to_owned(),
                conf,
                lines,
                size,
            })
        })
        .collect()
}

#[derive(Default)]
struct ReferenceIndex {
    code: HashMap<String, Vec<Reference>>,
    docs: HashMap<String, Vec<Reference>>,
}

impl ReferenceIndex {
    fn add(&mut self, path: &str, hits: Vec<python::Hit>, documentation: bool) {
        let index = if documentation {
            &mut self.docs
        } else {
            &mut self.code
        };
        for (line, name, kind) in hits {
            index
                .entry(name)
                .or_default()
                .push((path.to_owned(), line, kind.to_owned()));
        }
    }

    fn row(&self, finding: Finding) -> Row {
        let (mut prod, mut tests): (Vec<_>, Vec<_>) = self
            .code
            .get(&finding.name)
            .into_iter()
            .flatten()
            .filter(|reference| reference.0 != finding.file)
            .cloned()
            .partition(|reference| !is_test(&reference.0));
        let mut docs: Vec<_> = self
            .docs
            .get(&finding.name)
            .into_iter()
            .flatten()
            .filter(|reference| reference.0 != finding.file)
            .cloned()
            .collect();
        prod.sort();
        tests.sort();
        docs.sort();
        let verdict = if !prod.is_empty() {
            "ALIVE"
        } else if !tests.is_empty() {
            "TEST-ONLY"
        } else if !docs.is_empty() {
            "DOC-ONLY"
        } else {
            "DEAD"
        };
        Row {
            finding,
            verdict: verdict.to_owned(),
            prod,
            tests,
            docs,
        }
    }
}

fn is_test(path: &str) -> bool {
    path.split('/').any(|part| part == "tests")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("test_"))
}

/// Scan once for all candidate names and retain the exact reference evidence.
/// `excluded_paths` may name output files or directories, absolute or root-relative.
pub fn adjudicate(
    root: &Path,
    mut findings: Vec<Finding>,
    excluded_paths: &[PathBuf],
) -> Result<AuditReport> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot open root {}", root.display()))?;
    if !root.is_dir() {
        bail!("audit root is not a directory: {}", root.display());
    }
    for finding in &mut findings {
        finding.file = scan::normalize_finding_path(&root, &finding.file)?;
    }
    let names: HashSet<_> = findings
        .iter()
        .map(|finding| finding.name.clone())
        .collect();
    let mut report = AuditReport {
        schema_version: 1,
        complete: true,
        counts: BTreeMap::new(),
        findings: Vec::new(),
        diagnostics: Vec::new(),
        scanned_files: 0,
    };
    let mut index = ReferenceIndex::default();
    if !names.is_empty() {
        scan::repository(&root, &names, excluded_paths, &mut index, &mut report)?;
    }
    for finding in findings {
        let row = index.row(finding);
        *report.counts.entry(row.verdict.clone()).or_default() += 1;
        report.findings.push(row);
    }
    report
        .diagnostics
        .sort_by(|left, right| (&left.path, &left.message).cmp(&(&right.path, &right.message)));
    report.complete = report.diagnostics.is_empty();
    Ok(report)
}

mod python {
    use ruff_python_ast as ast;
    use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
    use ruff_text_size::Ranged;
    use std::collections::HashSet;
    use unicode_general_category::{get_general_category, GeneralCategory};

    pub(super) type Hit = (usize, String, &'static str);

    /// CPython regex `\w` means Unicode letters/numbers or underscore. Rust's regex
    /// `\w` additionally includes marks/join controls, so use the exact categories.
    fn word(character: char) -> bool {
        if character.is_ascii() {
            return character.is_ascii_alphanumeric() || character == '_';
        }
        character == '_'
            || matches!(
                get_general_category(character),
                GeneralCategory::UppercaseLetter
                    | GeneralCategory::LowercaseLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::DecimalNumber
                    | GeneralCategory::LetterNumber
                    | GeneralCategory::OtherNumber
            )
    }

    pub(super) fn text_hits(
        text: &str,
        names: &HashSet<String>,
        line: usize,
        kind: &'static str,
    ) -> Vec<Hit> {
        text.split(|character| !word(character))
            .filter(|token| names.contains(*token))
            .map(|token| (line, token.to_owned(), kind))
            .collect()
    }

    struct Collector<'a> {
        source: &'a str,
        names: &'a HashSet<String>,
        starts: Vec<usize>,
        hits: Vec<Hit>,
    }

    impl Collector<'_> {
        fn line(&self, range: ruff_text_size::TextRange) -> usize {
            self.starts
                .partition_point(|start| *start <= range.start().to_usize())
        }

        fn hit(&mut self, name: &str, range: ruff_text_size::TextRange, kind: &'static str) {
            if self.names.contains(name) {
                self.hits.push((self.line(range), name.to_owned(), kind));
            }
        }

        fn string(&mut self, value: &str, range: ruff_text_size::TextRange) {
            self.hits
                .extend(text_hits(value, self.names, self.line(range), "string"));
        }

        fn fstring(&mut self, string: &ast::ExprFString) {
            let mut literal = String::new();
            let mut range = string.range;
            for part in string.value.iter() {
                match part {
                    ast::FStringPart::Literal(part) => {
                        if literal.is_empty() {
                            range = part.range;
                        }
                        literal.push_str(&part.value);
                    }
                    ast::FStringPart::FString(part) => {
                        self.elements(&part.elements, &mut literal, &mut range);
                    }
                }
            }
            self.string(&literal, range);
        }

        fn elements(
            &mut self,
            elements: &ast::InterpolatedStringElements,
            literal: &mut String,
            range: &mut ruff_text_size::TextRange,
        ) {
            for element in elements {
                match element {
                    ast::InterpolatedStringElement::Literal(element) => {
                        if literal.is_empty() {
                            *range = element.range;
                        }
                        literal.push_str(&element.value);
                    }
                    ast::InterpolatedStringElement::Interpolation(element) => {
                        if let Some(debug) = &element.debug_text {
                            if literal.is_empty() {
                                *range = element.range;
                            }
                            literal.push_str(&debug.leading);
                            let expression = element.expression.range();
                            literal.push_str(
                                &self.source
                                    [expression.start().to_usize()..expression.end().to_usize()],
                            );
                            literal.push_str(&debug.trailing);
                        }
                        self.string(literal, *range);
                        literal.clear();
                        self.visit_expr(&element.expression);
                        if let Some(spec) = &element.format_spec {
                            let mut text = String::new();
                            let mut location = spec.range;
                            self.elements(&spec.elements, &mut text, &mut location);
                            self.string(&text, location);
                        }
                    }
                }
            }
        }
    }

    impl<'a> SourceOrderVisitor<'a> for Collector<'_> {
        fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
            match statement {
                ast::Stmt::FunctionDef(function) => {
                    self.hit(function.name.as_str(), function.name.range(), "def")
                }
                ast::Stmt::ClassDef(class) => {
                    self.hit(class.name.as_str(), class.name.range(), "class")
                }
                ast::Stmt::ImportFrom(import) => {
                    for alias in &import.names {
                        self.hit(alias.name.as_str(), import.range, "import");
                        if let Some(name) = &alias.asname {
                            self.hit(name.as_str(), import.range, "import-alias");
                        }
                    }
                }
                ast::Stmt::Import(import) => {
                    for alias in &import.names {
                        let original = alias.name.as_str().split('.').next().unwrap_or_default();
                        self.hit(original, import.range, "import");
                        if let Some(name) = &alias.asname {
                            let local = name.as_str().split('.').next().unwrap_or_default();
                            if local != original {
                                self.hit(local, import.range, "import");
                            }
                        }
                    }
                }
                _ => {}
            }
            walk_stmt(self, statement);
        }

        fn visit_expr(&mut self, expression: &'a ast::Expr) {
            match expression {
                ast::Expr::Name(name) => self.hit(name.id.as_str(), name.range, "name"),
                ast::Expr::Attribute(attribute) => {
                    self.hit(attribute.attr.as_str(), attribute.range, "attr")
                }
                ast::Expr::StringLiteral(string) => {
                    self.string(string.value.to_str(), string.range);
                    return;
                }
                ast::Expr::FString(string) => {
                    self.fstring(string);
                    return;
                }
                _ => {}
            }
            walk_expr(self, expression);
        }
    }

    pub(super) fn collect(source: &str, names: &HashSet<String>) -> Result<Vec<Hit>, String> {
        let module = ruff_python_parser::parse_module(source)
            .map_err(|error| error.to_string())?
            .into_syntax();
        let mut starts = vec![0];
        let bytes = source.as_bytes();
        starts.extend(bytes.iter().enumerate().filter_map(|(index, byte)| {
            (*byte == b'\n' || (*byte == b'\r' && bytes.get(index + 1) != Some(&b'\n')))
                .then_some(index + 1)
        }));
        let mut collector = Collector {
            source,
            names,
            starts,
            hits: Vec::new(),
        };
        for statement in &module.body {
            collector.visit_stmt(statement);
        }
        Ok(collector.hits)
    }
}

mod scan {
    use super::{python, AuditReport, Diagnostic, ReferenceIndex};
    use anyhow::{bail, Result};
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Component, Path, PathBuf};

    const SKIP_DIRS: &[&str] = &[
        ".venv",
        ".git",
        "node_modules",
        "__pycache__",
        ".mypy_cache",
        ".ruff_cache",
        ".pytest_cache",
        "dist",
        "build",
        ".agent_hooks",
        ".gemini",
        ".code-review-graph",
        "target",
        ".cache",
    ];
    const SKIP_PREFIX: &[&str] = &[
        "research/reports/",
        "research/tmp/",
        "research/cache/",
        "tasks/audit/",
        ".current_work",
    ];
    const CODE_EXT: &[&str] = &[
        "rs", "sh", "json", "toml", "yaml", "yml", "cfg", "ipynb", "cpp", "h", "cu",
    ];

    fn relative(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn lexical(path: &Path) -> Result<PathBuf> {
        let mut clean = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !clean.pop() {
                        bail!("path escapes repository: {}", path.display());
                    }
                }
                _ => clean.push(part.as_os_str()),
            }
        }
        Ok(clean)
    }

    pub(super) fn normalize_finding_path(root: &Path, file: &str) -> Result<String> {
        let path = lexical(Path::new(file))?;
        let path = if path.is_absolute() {
            path.strip_prefix(root)
                .map_err(|_| anyhow::anyhow!("finding outside repository: {file}"))?
        } else {
            path.as_path()
        };
        Ok(relative(path))
    }

    fn excluded(path: &Path, excludes: &[PathBuf]) -> bool {
        excludes.iter().any(|excluded| path.starts_with(excluded))
    }

    fn skip(path: &Path) -> bool {
        let relative = relative(path);
        path.components()
            .any(|part| SKIP_DIRS.iter().any(|name| part.as_os_str() == *name))
            || SKIP_PREFIX.iter().any(|prefix| {
                relative.starts_with(prefix) || relative == prefix.trim_end_matches('/')
            })
            || relative == "research/tools/vulture_whitelist.py"
    }

    fn diagnostic(report: &mut AuditReport, path: &Path, message: impl ToString) {
        report.diagnostics.push(Diagnostic {
            path: relative(path),
            message: message.to_string(),
        });
    }

    fn walk(
        root: &Path,
        directory: &Path,
        excludes: &[PathBuf],
        files: &mut Vec<PathBuf>,
        report: &mut AuditReport,
    ) {
        let entries = match fs::read_dir(root.join(directory)) {
            Ok(entries) => entries,
            Err(error) => {
                diagnostic(report, directory, format!("cannot list directory: {error}"));
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostic(
                        report,
                        directory,
                        format!("cannot inspect directory entry: {error}"),
                    );
                    continue;
                }
            };
            let path = directory.join(entry.file_name());
            if skip(&path) || excluded(&root.join(&path), excludes) {
                continue;
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    diagnostic(report, &path, format!("cannot read file type: {error}"));
                    continue;
                }
            };
            if kind.is_symlink() {
                match root.join(&path).canonicalize() {
                    Ok(target) if target.starts_with(root) && target.is_file() => {
                        if !excluded(&target, excludes) {
                            files.push(path);
                        }
                    }
                    Ok(target) if target.is_dir() => {}
                    Ok(_) if eligible(&path) => diagnostic(
                        report,
                        &path,
                        "file symlink points outside repository; skipped",
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        diagnostic(report, &path, format!("cannot resolve symlink: {error}"))
                    }
                }
            } else if kind.is_dir() {
                walk(root, &path, excludes, files, report);
            } else if kind.is_file() {
                files.push(path);
            }
        }
    }

    fn text_references(
        text: &str,
        names: &HashSet<String>,
        kind: &'static str,
    ) -> Vec<python::Hit> {
        let mut hits = Vec::new();
        let mut start = 0;
        let mut line = 1;
        let mut previous_cr = false;
        for (offset, character) in text.char_indices() {
            if previous_cr && character == '\n' {
                start = offset + 1;
                previous_cr = false;
                continue;
            }
            previous_cr = character == '\r';
            if matches!(
                character,
                '\n' | '\r'
                    | '\u{b}'
                    | '\u{c}'
                    | '\u{1c}'
                    | '\u{1d}'
                    | '\u{1e}'
                    | '\u{85}'
                    | '\u{2028}'
                    | '\u{2029}'
            ) {
                hits.extend(python::text_hits(&text[start..offset], names, line, kind));
                start = offset + character.len_utf8();
                line += 1;
            }
        }
        hits.extend(python::text_hits(&text[start..], names, line, kind));
        hits
    }

    fn eligible(path: &Path) -> bool {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        matches!(extension, "py" | "md" | "txt") || CODE_EXT.contains(&extension)
    }

    fn shell_shebang(text: &str) -> bool {
        let Some(shebang) = text.lines().next().and_then(|line| line.strip_prefix("#!")) else {
            return false;
        };
        let words: Vec<_> = shebang.split_whitespace().collect();
        let interpreter = words
            .first()
            .and_then(|word| word.rsplit('/').next())
            .unwrap_or_default();
        let interpreter = if interpreter == "env" {
            words.get(1).copied().unwrap_or_default()
        } else {
            interpreter
        };
        matches!(interpreter, "sh" | "bash" | "dash" | "zsh" | "ksh")
    }

    fn scan_file(
        root: &Path,
        path: &Path,
        names: &HashSet<String>,
        index: &mut ReferenceIndex,
        report: &mut AuditReport,
    ) {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let documentation = matches!(extension, "md" | "txt");
        if !eligible(path) {
            return;
        }
        let bytes = match fs::read(root.join(path)) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostic(report, path, format!("cannot read file: {error}"));
                return;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        if matches!(text, std::borrow::Cow::Owned(_)) {
            diagnostic(report, path, "invalid UTF-8 replaced during scan");
        }
        report.scanned_files += 1;
        let hits = if extension == "py" && !shell_shebang(&text) {
            match python::collect(&text, names) {
                Ok(hits) => hits,
                Err(error) => {
                    diagnostic(report, path, format!("Python parse failed: {error}"));
                    return;
                }
            }
        } else {
            text_references(&text, names, if documentation { "prose" } else { "text" })
        };
        index.add(&relative(path), hits, documentation);
    }

    pub(super) fn repository(
        root: &Path,
        names: &HashSet<String>,
        excluded_paths: &[PathBuf],
        index: &mut ReferenceIndex,
        report: &mut AuditReport,
    ) -> Result<()> {
        let excludes: Vec<_> = excluded_paths
            .iter()
            .map(|path| {
                lexical(&if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(path)
                })
            })
            .collect::<Result<_>>()?;
        let mut files = Vec::new();
        walk(root, Path::new(""), &excludes, &mut files, report);
        files.sort();
        for path in files {
            scan_file(root, &path, names, index, report);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "forge-dead-code-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn audit(&self, names: &[&str]) -> AuditReport {
            adjudicate(
                &self.0,
                names.iter().map(|name| finding(name)).collect(),
                &[],
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn finding(name: &str) -> Finding {
        Finding {
            file: "subject.py".to_owned(),
            line: 1,
            kind: "function".to_owned(),
            name: name.to_owned(),
            conf: 60,
            lines: None,
            size: 1,
        }
    }

    fn references(source: &str, names: &[&str]) -> Vec<python::Hit> {
        let names = names.iter().map(|name| name.to_string()).collect();
        python::collect(source, &names).unwrap()
    }

    #[test]
    fn dead_code_vulture_size_and_validation() {
        let output = "a.py:10: unused function 'alpha' (60% confidence, 12 lines)\nC:\\src\\b.py:2: unused variable 'beta' (100% confidence)\n";
        let findings = parse_vulture_output(output).unwrap();
        assert_eq!(findings[0].size, 12);
        assert_eq!(findings[0].lines.as_deref(), Some("12"));
        assert_eq!(findings[1].file, "C:\\src\\b.py");
        assert_eq!(findings[1].size, 1);
        assert!(findings[1].lines.is_none());
        assert!(parse_vulture_output("an error was printed").is_err());
        assert!(parse_vulture_output("a.py:0: unused variable 'bad' (60% confidence)").is_err());
        assert!(parse_vulture_output("a.py:1: unused variable 'bad' (101% confidence)").is_err());
        assert!(parse_vulture_output("\n").unwrap().is_empty());
    }

    #[test]
    fn dead_code_preserves_all_verdicts_and_precedence() {
        let fixture = Fixture::new();
        fixture.write("subject.py", "dead()\n");
        fixture.write("prod.py", "alive()\n");
        fixture.write("tests/test_usage.py", "alive()\ntested()\n");
        fixture.write("guide.md", "alive tested documented\n");
        let report = fixture.audit(&["alive", "tested", "documented", "dead"]);
        assert!(report.complete);
        assert_eq!(report.scanned_files, 4);
        assert_eq!(
            report
                .findings
                .iter()
                .map(|row| row.verdict.as_str())
                .collect::<Vec<_>>(),
            ["ALIVE", "TEST-ONLY", "DOC-ONLY", "DEAD"]
        );
        assert_eq!(
            report.findings[0].prod,
            [("prod.py".to_owned(), 1, "name".to_owned())]
        );
        assert!(report.findings[3].prod.is_empty());
    }

    #[test]
    fn dead_code_original_imports_aliases_and_dotted_attributes() {
        let hits = references("from module import original as alias\nimport pkg.deep as local\nimport same as same\nobj.target()\n", &["original", "alias", "pkg", "local", "same", "target"]);
        assert!(hits.contains(&(1, "original".to_owned(), "import")));
        assert!(hits.contains(&(1, "alias".to_owned(), "import-alias")));
        assert!(hits.contains(&(2, "pkg".to_owned(), "import")));
        assert!(hits.contains(&(2, "local".to_owned(), "import")));
        assert_eq!(hits.iter().filter(|hit| hit.1 == "same").count(), 1);
        assert!(hits.contains(&(4, "target".to_owned(), "attr")));
    }

    #[test]
    fn dead_code_decoded_strings_fstrings_and_boundaries() {
        let hits = references("x = ('tar' 'get', '\\x74arget', 'pkg.target:target', b'target', 'atarget target2 étarget targeté target_')\ny = f'pkg.target {other} tar' 'get'\nz = f'{target=}'\n", &["target"]);
        assert_eq!(
            hits.iter()
                .filter(|hit| hit.0 == 1 && hit.2 == "string")
                .count(),
            4
        );
        assert_eq!(
            hits.iter()
                .filter(|hit| hit.0 == 2 && hit.2 == "string")
                .count(),
            2
        );
        assert_eq!(
            hits.iter()
                .filter(|hit| hit.0 == 3 && hit.2 == "string")
                .count(),
            1
        );
        assert!(hits.contains(&(3, "target".to_owned(), "name")));
        let names = ["target".to_owned()].into_iter().collect();
        assert_eq!(
            python::text_hits("target\u{301} \u{200d}target ²target", &names, 1, "text").len(),
            2
        );
    }

    #[test]
    fn dead_code_definitions_stores_and_decorator_lines() {
        let hits = references("@register(\n    'pkg.target'\n)\nasync def target(parameter):\n    target = 1\nclass Other:\n    pass\n", &["target", "parameter", "Other"]);
        assert!(hits.contains(&(2, "target".to_owned(), "string")));
        assert!(hits.contains(&(4, "target".to_owned(), "def")));
        assert!(hits.contains(&(5, "target".to_owned(), "name")));
        assert!(hits.contains(&(6, "Other".to_owned(), "class")));
        assert!(!hits.iter().any(|hit| hit.1 == "parameter"));
    }

    #[test]
    fn dead_code_complete_evidence_no_caps_and_stable_order() {
        let fixture = Fixture::new();
        for number in 0..12 {
            fixture.write(&format!("p{number}.py"), "target()\n");
            fixture.write(&format!("tests/t{number}.py"), "target()\n");
            fixture.write(&format!("doc{number}.md"), "target\n");
        }
        let first = fixture.audit(&["target"]);
        let second = fixture.audit(&["target"]);
        assert_eq!(first, second);
        let row = &first.findings[0];
        assert_eq!(
            (row.prod.len(), row.tests.len(), row.docs.len()),
            (12, 12, 12)
        );
        assert!(row.prod.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn dead_code_skips_whitelist_cache_and_report_outputs() {
        let fixture = Fixture::new();
        for path in [
            ".venv/m.py",
            "node_modules/m.py",
            "target/m.py",
            ".cache/m.py",
            "research/reports/m.py",
            "research/tmp/m.py",
            "research/cache/m.py",
            "tasks/audit/m.py",
            "research/tools/vulture_whitelist.py",
            ".current_work.md",
        ] {
            fixture.write(path, "target()\n");
        }
        fixture.write("audit.json", "target");
        let report = adjudicate(
            &fixture.0,
            vec![finding("target")],
            &[PathBuf::from("audit.json")],
        )
        .unwrap();
        assert!(report.complete);
        assert_eq!(report.scanned_files, 0);
        assert_eq!(report.findings[0].verdict, "DEAD");
    }

    #[test]
    fn dead_code_invalid_python_retains_incomplete_evidence() {
        let fixture = Fixture::new();
        fixture.write("broken.py", "def broken(\n");
        let report = fixture.audit(&["target"]);
        assert!(!report.complete);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].path, "broken.py");
        assert!(report.diagnostics[0]
            .message
            .contains("Python parse failed"));
    }

    #[test]
    fn dead_code_shell_shebang_is_explicit_text_evidence() {
        let fixture = Fixture::new();
        fixture.write(
            "hook.py",
            "#!/usr/bin/env bash\nset -euo pipefail\necho target\n",
        );
        let report = fixture.audit(&["target"]);
        assert!(report.complete);
        assert_eq!(
            report.findings[0].prod,
            [("hook.py".to_owned(), 3, "text".to_owned())]
        );
    }

    #[test]
    fn dead_code_text_lines_and_test_detection() {
        let fixture = Fixture::new();
        fixture.write("test_native.rs", "target\r\ntarget\rtarget\u{2028}target");
        fixture.write("testing/readme.md", "target");
        let report = fixture.audit(&["target"]);
        let row = &report.findings[0];
        assert_eq!(row.verdict, "TEST-ONLY");
        assert_eq!(
            row.tests
                .iter()
                .map(|reference| reference.1)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(row.docs.len(), 1);
        assert!(is_test("a/tests/helpers.py"));
        assert!(!is_test("testing/helpers.py"));
        assert!(!is_test("a/tests_extra/helpers.py"));
    }

    #[test]
    fn dead_code_no_findings_and_normalized_own_file() {
        let fixture = Fixture::new();
        fixture.write("broken.py", "not valid Python !");
        let report = fixture.audit(&[]);
        assert!(report.complete);
        assert_eq!(report.scanned_files, 0);
        assert!(report.counts.is_empty());
        fs::remove_file(fixture.0.join("broken.py")).unwrap();
        fixture.write("subject.py", "target()\n");
        let mut input = finding("target");
        input.file = fixture.0.join("subject.py").to_string_lossy().into_owned();
        let report = adjudicate(&fixture.0, vec![input], &[]).unwrap();
        assert_eq!(report.findings[0].finding.file, "subject.py");
        assert_eq!(report.findings[0].verdict, "DEAD");
        let mut escaped = finding("target");
        escaped.file = "../outside.py".to_owned();
        assert!(adjudicate(&fixture.0, vec![escaped], &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dead_code_external_file_symlink_is_incomplete() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.write("source.py", "target()\n");
        std::os::unix::fs::symlink(outside.0.join("source.py"), fixture.0.join("linked.py"))
            .unwrap();
        let report = fixture.audit(&["target"]);
        assert!(!report.complete);
        assert!(report.diagnostics[0].message.contains("outside repository"));
    }
}
