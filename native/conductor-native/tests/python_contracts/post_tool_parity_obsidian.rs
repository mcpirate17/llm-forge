//! Obsidian PostToolUse corpus fixtures, including mirror and journal normalization.

use crate::comm_support::{buffer_text, capture, py_json};
use crate::post_tool_support::{
    replace_strings, repo_root, substitute, DATE_STAMP, MANAGED_ENV, STAMP,
};
use crate::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn slug(repo: &Path) -> String {
    repo.to_string_lossy()
        .replace('/', "-")
        .replace(['_', '.'], "-")
}

fn normalized_accum(text: &str, mapping: &[(String, String)]) -> String {
    let parts = text
        .trim_end_matches('\n')
        .splitn(3, '\t')
        .collect::<Vec<_>>();
    assert_eq!(parts.len(), 3);
    let fingerprint = replace_strings(Value::String(parts[2].to_owned()), mapping);
    format!("{STAMP}\t{}\t{}", parts[1], fingerprint.as_str().unwrap())
}

fn normalized_mirror(text: &str, mapping: &[(String, String)]) -> String {
    let mut replaced_date = false;
    let dated = text
        .split_inclusive('\n')
        .map(|line| {
            if !replaced_date && line.starts_with("date: ") {
                replaced_date = true;
                format!(
                    "date: {DATE_STAMP}{}",
                    if line.ends_with('\n') { "\n" } else { "" }
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<String>();
    replace_strings(Value::String(dated), mapping)
        .as_str()
        .unwrap()
        .to_owned()
}

fn fresh_module<'py>(py: Python<'py>, index: usize) -> Bound<'py, pyo3::types::PyAny> {
    let importlib = module(py, "importlib.util");
    let source = repo_root().join("src/tooling/hooks/claude/obsidian_sync.py");
    let spec = importlib
        .getattr("spec_from_file_location")
        .unwrap()
        .call1((
            format!("obsidian_sync_twin_{index}"),
            source.to_str().unwrap(),
        ))
        .unwrap();
    let loaded = importlib
        .getattr("module_from_spec")
        .unwrap()
        .call1((&spec,))
        .unwrap();
    spec.getattr("loader")
        .unwrap()
        .call_method1("exec_module", (&loaded,))
        .unwrap();
    loaded
}

fn invoke(case: &Value, payload: Value, index: usize) -> (Value, PathBuf) {
    Python::attach(|py| {
        let obsidian = fresh_module(py, index);
        let sys = module(py, "sys");
        let json = module(py, "json");
        let input_text: String = json
            .getattr("dumps")
            .unwrap()
            .call1((py_json(py, payload),))
            .unwrap()
            .extract()
            .unwrap();
        let input = module(py, "io")
            .getattr("StringIO")
            .unwrap()
            .call1((input_text,))
            .unwrap();
        let _stdin = AttrPatch::replace(sys.as_any(), "stdin", input.as_any());
        let (stdout, _capture) = capture(py, "stdout");
        obsidian.getattr("cmd_post_edit").unwrap().call0().unwrap();
        let text = buffer_text(&stdout);
        let output = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(text.trim()).unwrap()
        };
        let vault: String = obsidian
            .getattr("VAULT_ROOT")
            .unwrap()
            .str()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(case["kind"], "obsidian_edit");
        (output, PathBuf::from(vault))
    })
}

pub fn run(case: &Value, parent: &Path, env: &mut Case, index: usize) -> Value {
    let label = case["id"].as_str().unwrap();
    let repo = parent.join(format!("{label}-repo"));
    let vault = parent.join(format!("{label}-vault"));
    let mem = parent.join(format!("{label}-mem"));
    let home = parent.join(format!("{label}-home"));
    for directory in [&repo, &vault, &mem, &home] {
        fs::create_dir(directory).unwrap();
    }
    let default_mem = home
        .join(".claude/projects")
        .join(slug(&repo))
        .join("memory");
    let where_ = case["seed"]["where"].as_str().unwrap_or("repo");
    let base = match where_ {
        "repo" => &repo,
        "mem" => &mem,
        "mem-default" => &default_mem,
        other => panic!("unknown memory root {other}"),
    };
    let payload = if let Some(name) = case["seed"]["file"].as_str() {
        fs::create_dir_all(base).unwrap();
        let file = base.join(name);
        if let Some(content) = case["seed"]["content"].as_str() {
            fs::write(&file, content).unwrap();
        }
        substitute(
            case["payload"].clone(),
            &[("<FILE>".to_owned(), file.to_string_lossy().into_owned())],
        )
    } else {
        case["payload"].clone()
    };
    for (key, placeholder) in case["env"].as_object().unwrap() {
        let name: &'static str = MANAGED_ENV
            .iter()
            .copied()
            .find(|name| name == key)
            .unwrap();
        let value = match placeholder.as_str().unwrap() {
            "<VAULT>" => &vault,
            "<MEM>" => &mem,
            "<HOME>" => &home,
            other => panic!("unknown Obsidian placeholder {other}"),
        };
        env.set_env(name, value.to_str().unwrap());
    }
    if case["env"].get("CLAUDE_PROJECT_DIR").is_none() {
        env.set_env("CLAUDE_PROJECT_DIR", repo.to_str().unwrap());
    }
    let session = case["payload"]["session_id"].as_str().unwrap();
    let accum_path = Path::new("/tmp/claude-session-journal").join(format!("{session}.tsv"));
    if accum_path.exists() {
        fs::remove_file(&accum_path).unwrap();
    }
    let (output, vault_root) = invoke(case, payload, index);
    let mapping = vec![
        (home.to_string_lossy().into_owned(), "<H>".to_owned()),
        (vault.to_string_lossy().into_owned(), "<V>".to_owned()),
        (mem.to_string_lossy().into_owned(), "<M>".to_owned()),
        (repo.to_string_lossy().into_owned(), "<R>".to_owned()),
        (slug(&repo), "<RS>".to_owned()),
    ];
    let accum = if accum_path.exists() {
        let text = fs::read_to_string(&accum_path).unwrap();
        fs::remove_file(&accum_path).unwrap();
        Value::String(normalized_accum(&text, &mapping))
    } else {
        Value::Null
    };
    let (mirror_path, mirror) = mirror(&vault_root, &mapping);
    json!({"output": output, "accum": accum, "mirror_path": mirror_path, "mirror": mirror})
}

fn mirror(vault_root: &Path, mapping: &[(String, String)]) -> (Value, Value) {
    let notes = vault_root.join("memory");
    if !notes.is_dir() {
        return (Value::Null, Value::Null);
    }
    let mut files = fs::read_dir(notes)
        .unwrap()
        .map(|item| item.unwrap().path())
        .filter(|file| file.extension().is_some_and(|extension| extension == "md"))
        .collect::<Vec<_>>();
    files.sort();
    if let Some(file) = files.first() {
        let relative = file
            .strip_prefix(vault_root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let content = normalized_mirror(&fs::read_to_string(file).unwrap(), mapping);
        (Value::String(relative), Value::String(content))
    } else {
        (Value::Null, Value::Null)
    }
}
