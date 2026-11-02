//! Native analysis over held candidate blobs; never scans the live worktree.
#[path = "candidate_analysis_inputs.rs"]
mod inputs;
use crate::{
    candidate_policy::Policy,
    candidate_source::{lines::LineRange, Candidate, Source},
};
use anyhow::{ensure, Context, Result};
use clap::ValueEnum;
use conductor_native::candidate_policy::{GlobWorkspace, PreparedGlob};
pub(crate) use inputs::validate_operator_provider;
use inputs::{Capture, Provider, MAX_TOTAL};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    ConfigParse,
    PythonAst,
    ImportDeclaration,
    DuplicateFunctionBodies,
    StructureAudit,
}
impl Mode {
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "config-parse" => Some(Self::ConfigParse),
            "python-ast" => Some(Self::PythonAst),
            "import-declaration" => Some(Self::ImportDeclaration),
            "duplicate-function-bodies" => Some(Self::DuplicateFunctionBodies),
            "structure-audit" => Some(Self::StructureAudit),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::ConfigParse => "config-parse",
            Self::PythonAst => "python-ast",
            Self::ImportDeclaration => "import-declaration",
            Self::DuplicateFunctionBodies => "duplicate-function-bodies",
            Self::StructureAudit => "structure-audit",
        }
    }
    fn peers(self) -> bool {
        matches!(self, Self::DuplicateFunctionBodies | Self::StructureAudit)
    }
}

pub struct Plan {
    capture: Capture,
    provider: Option<Provider>,
    payload: Value,
    pub report: Value,
    pub files: Vec<String>,
    mode: Mode,
}

fn skipped_peer(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            ".git" | ".venv" | "node_modules" | "__pycache__" | ".run"
        )
    })
}
pub(crate) fn select(
    candidate: &Candidate,
    mode: Mode,
    selected: &[String],
) -> Result<(Vec<String>, Vec<Value>)> {
    ensure!(
        !selected.is_empty() && selected.len() <= 4096,
        "analysis requires 1..4096 selected changes"
    );
    let changes: BTreeMap<_, _> = candidate
        .changes
        .iter()
        .map(|change| (change.path.as_str(), change))
        .collect();
    let mut seen = BTreeSet::new();
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for path in selected {
        crate::candidate_source::safe_path(path.as_bytes())?;
        ensure!(
            path.len() <= 4096
                && !path.chars().any(char::is_control)
                && path.split('/').count() <= 128,
            "invalid analysis path"
        );
        ensure!(seen.insert(path), "duplicate selected analysis path");
        let change = changes
            .get(path.as_str())
            .context("selected analysis path is not a captured change")?;
        let class = |name: &str| change.classes.iter().any(|class| class == name);
        let eligible = if mode == Mode::ConfigParse {
            ["config", "notebook", "workflow"]
                .iter()
                .any(|name| class(name))
        } else {
            class("python")
        };
        let reason = if change.deleted() {
            Some("deleted")
        } else if !eligible {
            Some("class-not-selected")
        } else if mode == Mode::ImportDeclaration && class("test") {
            Some("test-class")
        } else if mode.peers() && skipped_peer(path) {
            Some("peer-directory-excluded")
        } else {
            None
        };
        if let Some(reason) = reason {
            excluded.push(json!({"path":path,"reason":reason}));
        } else {
            included.push(path.clone());
        }
    }
    included.sort();
    excluded.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok((included, excluded))
}

fn changed_lines(
    source: &Source,
    candidate: &Candidate,
    files: &[String],
) -> Result<BTreeMap<String, Vec<LineRange>>> {
    if files.is_empty() {
        return Ok(BTreeMap::new());
    }
    let selected: Vec<_> = files.iter().map(String::as_str).collect();
    let ranges = source.changed_ranges(candidate, &selected)?;
    let mut result = BTreeMap::new();
    for file in ranges.files {
        result.insert(file.path, file.ranges);
    }
    Ok(result)
}

fn append_python_lines(
    result: &mut Vec<u32>,
    first: usize,
    last: usize,
    total: &mut usize,
) -> Result<()> {
    let first = first.max(result.last().copied().unwrap_or(0) as usize + 1);
    if first > last {
        return Ok(());
    }
    let count = last
        .checked_sub(first)
        .and_then(|v| v.checked_add(1))
        .context("Python line overflow")?;
    *total = total
        .checked_add(count)
        .context("analysis changed-line overflow")?;
    ensure!(
        *total <= 65_536,
        "analysis projected changed lines exceed 65536"
    );
    for line in first..=last {
        result.push(u32::try_from(line)?);
    }
    Ok(())
}

/// Git hunks count LF records; one record can contain several Python CR lines.
fn project_git_lines(source: &str, ranges: &[LineRange], total: &mut usize) -> Result<Vec<u32>> {
    if ranges.is_empty() {
        return Ok(Vec::new());
    }
    let mut last = 0u32;
    for item in ranges {
        ensure!(
            item.start > last && item.end >= item.start,
            "invalid Git line intervals"
        );
        last = item.end;
    }
    let lines = conductor_native::candidate_python::Lines::new(source);
    let mut result = Vec::new();
    let mut range = 0usize;
    let mut git_line = 1u32;
    let mut start = 0usize;
    for (offset, byte) in source.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        while range < ranges.len() && ranges[range].end < git_line {
            range += 1;
        }
        if range < ranges.len() && ranges[range].start <= git_line {
            append_python_lines(
                &mut result,
                lines.number(start),
                lines.number(offset),
                total,
            )?;
        }
        start = offset + 1;
        git_line = git_line.checked_add(1).context("Git line overflow")?;
    }
    if start < source.len() {
        while range < ranges.len() && ranges[range].end < git_line {
            range += 1;
        }
        if range < ranges.len() && ranges[range].start <= git_line {
            append_python_lines(
                &mut result,
                lines.number(start),
                lines.number(source.len() - 1),
                total,
            )?;
        }
    }
    let existing = if start < source.len() {
        git_line
    } else {
        git_line - 1
    };
    ensure!(last <= existing, "Git line interval exceeds source");
    Ok(result)
}

fn python_records(
    capture: &mut Capture,
    mode: Mode,
    files: &[String],
    candidate: &Candidate,
    policy: &Policy,
    lines: &BTreeMap<String, Vec<LineRange>>,
) -> Result<Vec<Value>> {
    let paths = if mode.peers() {
        capture
            .entries
            .keys()
            .filter(|path| path.ends_with(".py") && !skipped_peer(path))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        files.to_vec()
    };
    ensure!(
        paths.len() <= 4096,
        "analysis Python inventory exceeds 4096"
    );
    let classes: BTreeMap<_, _> = candidate
        .changes
        .iter()
        .map(|change| (&change.path, &change.classes))
        .collect();
    let globs = policy.document["hot_path_globs"]
        .as_array()
        .context("policy hot paths missing")?;
    let pattern_bytes: usize = globs.iter().filter_map(Value::as_str).map(str::len).sum();
    let path_bytes: usize = paths.iter().map(String::len).sum();
    ensure!(
        pattern_bytes.saturating_mul(path_bytes) <= 100_000_000,
        "analysis hot-path matching exceeds work budget"
    );
    let patterns: Vec<_> = globs
        .iter()
        .map(|value| {
            value
                .as_str()
                .context("invalid hot path")
                .map(PreparedGlob::new)
        })
        .collect::<Result<_>>()?;
    let mut matcher = GlobWorkspace::default();
    let mut records = Vec::new();
    let mut projected = 0usize;
    for path in paths {
        let text = capture.read(&path)?;
        let changed = project_git_lines(
            text,
            lines.get(&path).map_or(&[], Vec::as_slice),
            &mut projected,
        )?;
        let record = match mode {
            Mode::PythonAst => json!({"path":path,"text":text,"changed_lines":changed,
                "classes":classes.get(&path).copied().cloned().unwrap_or_default(),
                "hot":patterns.iter().any(|pattern|matcher.matches(&path,pattern))}),
            Mode::DuplicateFunctionBodies => {
                json!({"path":path,"text":text,"changed_lines":changed})
            }
            _ => json!({"path":path,"text":text}),
        };
        records.push(record);
    }
    Ok(records)
}

fn import_payload(
    capture: &mut Capture,
    files: &[String],
    provider: &Provider,
) -> Result<(Value, Vec<String>)> {
    let paths: Vec<_> = capture
        .entries
        .keys()
        .filter(|path| {
            Path::new(path).file_name().and_then(|name| name.to_str()) == Some("pyproject.toml")
        })
        .cloned()
        .collect();
    ensure!(
        paths.len() <= 4096,
        "analysis manifest inventory exceeds 4096"
    );
    let manifests = paths
        .iter()
        .map(|path| Ok(json!({"path":path,"text":capture.read(path)?})))
        .collect::<Result<Vec<_>>>()?;
    let manifest_paths: BTreeSet<_> = paths.iter().map(String::as_str).collect();
    let mut records = Vec::new();
    for path in files {
        let mut parent = Path::new(path).parent();
        let mut nearest = None;
        while let Some(directory) = parent {
            let candidate = directory.join("pyproject.toml");
            let candidate = candidate.to_str().context("manifest path must be UTF-8")?;
            if manifest_paths.contains(candidate) {
                nearest = Some(candidate.to_owned());
                break;
            }
            parent = directory.parent();
        }
        records.push(json!({"path":path,"text":capture.read(path)?,"manifest":nearest}));
    }
    Ok((
        json!({"files":records,"manifests":manifests,"strict_trees":capture.strict_trees()?,"providers":provider.payload}),
        paths,
    ))
}

impl Plan {
    pub fn new(
        source: &Source,
        candidate: &Candidate,
        root: &Path,
        policy: &Policy,
        mode: Mode,
        selected: &[String],
        provider_spec: Option<&str>,
    ) -> Result<Self> {
        let (files, excluded) = select(candidate, mode, selected)?;
        let mut capture = Capture::new(source, candidate, root, policy)?;
        for change in &candidate.changes {
            if change.deleted() && selected.contains(&change.path) {
                capture.absence(&change.path)?;
            }
        }
        let provider = if mode == Mode::ImportDeclaration && !files.is_empty() {
            Some(Provider::load(
                provider_spec
                    .context("import-declaration requires explicit FORGE_IMPORT_PROVIDERS")?,
                root,
            )?)
        } else {
            None
        };
        let mut manifest_paths = Vec::new();
        let payload = if files.is_empty() {
            json!({"files":[]})
        } else {
            match mode {
                Mode::ConfigParse => {
                    let records = files
                        .iter()
                        .map(|path| {
                            Ok(json!({"path":path,"text":capture.read(path)?,
                        "notebook":path.ends_with(".ipynb")}))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    json!({"files":records})
                }
                Mode::ImportDeclaration => {
                    let (payload, paths) = import_payload(
                        &mut capture,
                        &files,
                        provider.as_ref().context("provider not loaded")?,
                    )?;
                    manifest_paths = paths;
                    payload
                }
                _ => {
                    let lines = if matches!(mode, Mode::PythonAst | Mode::DuplicateFunctionBodies) {
                        changed_lines(source, candidate, &files)?
                    } else {
                        BTreeMap::new()
                    };
                    let records =
                        python_records(&mut capture, mode, &files, candidate, policy, &lines)?;
                    if mode == Mode::PythonAst {
                        json!({"files":records})
                    } else {
                        json!({"files":records,"changed":files})
                    }
                }
            }
        };
        crate::json_canon::encoded_size(&payload, MAX_TOTAL)?;
        let report = json!({"base_tree_oid":candidate.base_tree_oid,"candidate_tree_oid":candidate.tree_oid,
            "selected_files":selected,"included_files":files,"excluded_files":excluded,
            "policy_path":policy.path,"policy_sha256":policy.digest,"manifest_paths":manifest_paths,
            "provider_metadata":provider.as_ref().map(|provider|&provider.evidence),"source_bytes":capture.bytes});
        let plan = Self {
            capture,
            provider,
            payload,
            report,
            files,
            mode,
        };
        plan.verify()?;
        Ok(plan)
    }
    pub fn verify(&self) -> Result<()> {
        self.capture.verify()?;
        if let Some(provider) = &self.provider {
            provider.verify()?;
        }
        Ok(())
    }
    pub fn evaluate(&self) -> Result<Value> {
        if self.files.is_empty() {
            return Ok(json!({"findings":[],"files":[],"metrics":{"files_analyzed":0}}));
        }
        let result = match self.mode {
            Mode::ConfigParse => {
                let records = self.payload["files"]
                    .as_array()
                    .context("missing config records")?
                    .iter()
                    .map(|record| {
                        Ok(crate::candidate_config::ConfigInput {
                            path: record["path"]
                                .as_str()
                                .context("missing config path")?
                                .into(),
                            text: record["text"]
                                .as_str()
                                .context("missing config text")?
                                .into(),
                            notebook: record["notebook"] == true,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                crate::candidate_config::evaluate(&records)?
            }
            Mode::PythonAst => python_evaluate(&self.payload, &self.files)?,
            Mode::ImportDeclaration => {
                conductor_native::candidate_import_declaration::evaluate(&self.payload)
                    .map_err(anyhow::Error::msg)?
            }
            Mode::DuplicateFunctionBodies => {
                conductor_native::candidate_duplicate_policy::evaluate(&self.payload)
                    .map_err(anyhow::Error::msg)?
            }
            Mode::StructureAudit => conductor_native::candidate_structure::evaluate(&self.payload)
                .map_err(anyhow::Error::msg)?,
        };
        crate::json_canon::encoded_size(&result, MAX_TOTAL)?;
        self.verify()?;
        Ok(result)
    }
}

fn python_evaluate(payload: &Value, files: &[String]) -> Result<Value> {
    let mut findings = Vec::new();
    let mut bytes = 0usize;
    for file in payload["files"]
        .as_array()
        .context("missing Python records")?
    {
        let result = conductor_native::candidate_checks::evaluate("python-ast", file)
            .map_err(anyhow::Error::msg)?;
        let mut rows = result["findings"]
            .as_array()
            .context("missing Python findings")?
            .clone();
        ensure!(
            findings.len() + rows.len() <= 4096,
            "Python findings exceed 4096"
        );
        bytes = bytes
            .checked_add(crate::json_canon::encoded_size(&result, MAX_TOTAL)?)
            .context("Python result overflow")?;
        ensure!(bytes <= MAX_TOTAL, "Python output exceeds 32 MiB");
        findings.append(&mut rows);
    }
    Ok(json!({"findings":findings,"files":files,"metrics":{"files_analyzed":files.len()}}))
}

#[cfg(test)]
mod newline_tests {
    use super::*;
    #[test]
    fn git_lf_lines_project_to_all_contained_python_lines() {
        let range = |start, end| LineRange { start, end };
        let mut total = 0;
        assert_eq!(
            project_git_lines("a\rb\rc\r", &[range(1, 1)], &mut total).unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(
            project_git_lines("a\rb\nc\rd\n", &[range(2, 2)], &mut total).unwrap(),
            vec![3, 4]
        );
        assert_eq!(
            project_git_lines("a\r\nb\nc", &[range(1, 2)], &mut total).unwrap(),
            vec![1, 2]
        );
        assert_eq!(
            project_git_lines("a\n", &[range(1, 1)], &mut total).unwrap(),
            vec![1]
        );
        assert!(project_git_lines("a\n", &[range(2, 2)], &mut total).is_err());
    }
    #[test]
    fn projected_limit_counts_python_lines_before_expansion() {
        let source = "a\r".repeat(65_537);
        let mut total = 0;
        assert!(project_git_lines(&source, &[LineRange { start: 1, end: 1 }], &mut total).is_err());
    }
}
