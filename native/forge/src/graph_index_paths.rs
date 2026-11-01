//! Conservative literal-path dependency edges; no linker or bundler emulation.
use super::{insert_import_edge, IndexedFile, ModuleLookup};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Component, Path};

fn normalized(parent: &Path, target: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in parent.join(target).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => (),
            Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn candidates(path: &str, script: bool) -> Vec<String> {
    let mut paths = vec![path.to_owned()];
    if script {
        if Path::new(path).extension().is_none() {
            for extension in ["js", "ts", "tsx", "jsx"] {
                paths.push(format!("{path}.{extension}"));
                paths.push(format!("{path}/index.{extension}"));
            }
        } else if let Some(stem) = path.strip_suffix(".js") {
            paths.extend([format!("{stem}.ts"), format!("{stem}.tsx")]);
        }
    }
    paths
}

pub(super) fn write_edges(
    tx: &rusqlite::Transaction<'_>,
    file: &IndexedFile,
    lookup: &ModuleLookup,
) -> Result<bool> {
    let script = matches!(file.language, "javascript" | "typescript" | "tsx");
    let parent = Path::new(&file.path).parent().unwrap_or(Path::new(""));
    for target in &file.facts.path_imports {
        if script && !target.starts_with('.') {
            continue;
        }
        let mut matches = BTreeSet::new();
        for directory in [parent, Path::new("")] {
            let Some(path) = normalized(directory, target) else {
                continue;
            };
            for candidate in candidates(&path, script) {
                for &language in if script {
                    &["javascript", "typescript", "tsx"][..]
                } else {
                    &["c", "cpp"][..]
                } {
                    // Require a complete path match; module suffix aliases are not evidence.
                    if let Some(Some(found)) = lookup.get(&(language.to_owned(), candidate.clone()))
                    {
                        if found == &candidate {
                            matches.insert(found.clone());
                        }
                    }
                }
            }
            if script || !matches.is_empty() {
                break;
            }
        }
        if matches.len() == 1 {
            insert_import_edge(tx, &file.path, matches.first().unwrap())?;
        }
    }
    // Cross-file semantics require the build system's include paths, macro set,
    // linker inputs, bundler aliases and type checker. Keep selection conservative.
    Ok(true)
}
