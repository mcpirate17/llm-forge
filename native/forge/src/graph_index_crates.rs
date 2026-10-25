//! Rust crates as units of the import graph. A crate compiles every file under its
//! directory into one artifact, so its root file imports each of its files and the
//! root of each crate it depends on by path, and each Python file that imports the
//! module the crate builds imports the root. Inside a crate linked this way an
//! untraced call cannot hide a test: every file reaches the root, and the root reaches
//! every consumer. A crate the graph cannot close stays a reason to run every test.

use super::{insert_import_edge, IndexedFile};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use toml::{Table, Value};

/// Library crate types only another Rust crate can load.
const RUST_ONLY: &[&str] = &["lib", "rlib", "proc-macro"];
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "build-dependencies"];

struct Crate {
    /// The indexed file the crate is compiled from.
    root: Option<String>,
    /// The Python module the crate builds, when maturin builds it.
    module: Option<String>,
    /// No executable target, and every consumer is a file the graph sees.
    closed: bool,
    /// Directories of the crates it depends on by path.
    dependencies: Vec<String>,
}

pub(super) struct Crates {
    crates: BTreeMap<String, Crate>,
    /// Each indexed Rust file's crate directory; `None` outside every package.
    owner: BTreeMap<String, Option<String>>,
    /// Digest of every manifest read, so a manifest edit rebuilds the graph.
    pub(super) digest: String,
}

enum Manifest {
    Absent,
    Unparsed,
    Parsed(Table),
}

struct Manifests<'a> {
    host: &'a Path,
    read: BTreeMap<String, Manifest>,
    hashes: BTreeMap<String, String>,
}

fn join(dir: &str, relative: &str) -> String {
    if dir.is_empty() {
        relative.to_owned()
    } else {
        format!("{dir}/{relative}")
    }
}

/// `path` with `.` and `..` folded, or `None` when it is absolute or leaves the host.
fn normalize(path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// `dir` and each directory above it, up to the host root `""`.
fn ancestors(dir: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(dir), |dir| {
        (!dir.is_empty()).then(|| dir.rsplit_once('/').map_or("", |(parent, _)| parent))
    })
}

fn text<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Value::as_str)
}

fn table<'a>(table: &'a Table, key: &str) -> Option<&'a Table> {
    table.get(key).and_then(Value::as_table)
}

impl Manifests<'_> {
    fn get(&mut self, relative: &str) -> Result<&Manifest> {
        if !self.read.contains_key(relative) {
            let manifest = match fs::read_to_string(self.host.join(relative)) {
                Ok(source) => {
                    let hash = format!("{:x}", Sha256::digest(&source));
                    self.hashes.insert(relative.to_owned(), hash);
                    source.parse().map_or(Manifest::Unparsed, Manifest::Parsed)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Manifest::Absent,
                Err(error) => return Err(error).with_context(|| format!("read {relative}")),
            };
            self.read.insert(relative.to_owned(), manifest);
        }
        Ok(&self.read[relative])
    }

    fn parsed(&mut self, relative: &str) -> Result<Option<Table>> {
        Ok(match self.get(relative)? {
            Manifest::Parsed(table) => Some(table.clone()),
            _ => None,
        })
    }

    fn exists(&self, dir: &str, relative: &str) -> bool {
        self.host.join(join(dir, relative)).exists()
    }

    /// The nearest directory above `path` whose `Cargo.toml` is a package.
    fn package_of(&mut self, path: &str) -> Result<Option<String>> {
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        for dir in ancestors(parent) {
            let package = match self.get(&join(dir, "Cargo.toml"))? {
                Manifest::Absent => false,
                Manifest::Unparsed => true,
                Manifest::Parsed(manifest) => manifest.contains_key("package"),
            };
            if package {
                return Ok(Some(dir.to_owned()));
            }
        }
        Ok(None)
    }

    fn read_crate(&mut self, dir: &str, indexed: &BTreeSet<&str>) -> Result<Crate> {
        let unknown = Crate {
            root: None,
            module: None,
            closed: false,
            dependencies: Vec::new(),
        };
        let Some(cargo) = self.parsed(&join(dir, "Cargo.toml"))? else {
            return Ok(unknown);
        };
        let lib = table(&cargo, "lib");
        let lib_name = lib
            .and_then(|lib| text(lib, "name"))
            .map(str::to_owned)
            .or_else(|| {
                table(&cargo, "package")
                    .and_then(|package| text(package, "name"))
                    .map(|name| name.replace('-', "_"))
            });
        let root = [
            lib.and_then(|lib| text(lib, "path")),
            Some("src/lib.rs"),
            Some("src/main.rs"),
        ]
        .into_iter()
        .flatten()
        .filter_map(|path| normalize(&join(dir, path)))
        .find(|path| indexed.contains(path.as_str()));
        let module = self.module(dir, &cargo, lib_name)?;
        let types = lib
            .and_then(|lib| lib.get("crate-type"))
            .and_then(Value::as_array);
        let rust_only = types.is_none_or(|types| {
            types
                .iter()
                .all(|kind| kind.as_str().is_some_and(|kind| RUST_ONLY.contains(&kind)))
        });
        let executable = ["bin", "example"]
            .iter()
            .any(|key| cargo.contains_key(*key))
            || ["src/main.rs", "src/bin", "examples"]
                .iter()
                .any(|path| self.exists(dir, path));
        Ok(Crate {
            closed: root.is_some() && !executable && (module.is_some() || rust_only),
            root,
            module,
            dependencies: self.dependencies(dir, &cargo)?,
        })
    }

    /// The module maturin builds from the crate: a `pyproject.toml` at or above it that
    /// names its manifest, else `[package.metadata.maturin]`.
    fn module(
        &mut self,
        dir: &str,
        cargo: &Table,
        lib_name: Option<String>,
    ) -> Result<Option<String>> {
        let manifest = join(dir, "Cargo.toml");
        for ancestor in ancestors(dir) {
            let Some(pyproject) = self.parsed(&join(ancestor, "pyproject.toml"))? else {
                continue;
            };
            let backend =
                table(&pyproject, "build-system").and_then(|build| text(build, "build-backend"));
            let maturin = table(&pyproject, "tool").and_then(|tool| table(tool, "maturin"));
            if maturin.is_none() && backend != Some("maturin") {
                continue;
            }
            let named = maturin
                .and_then(|maturin| text(maturin, "manifest-path"))
                .unwrap_or("Cargo.toml");
            if normalize(&join(ancestor, named)).as_deref() == Some(manifest.as_str()) {
                let module = maturin.and_then(|maturin| text(maturin, "module-name"));
                return Ok(module.map(str::to_owned).or(lib_name));
            }
        }
        let metadata = table(cargo, "package")
            .and_then(|package| table(package, "metadata"))
            .and_then(|metadata| table(metadata, "maturin"));
        Ok(metadata.and_then(|maturin| match text(maturin, "name") {
            Some(name) => Some(name.to_owned()),
            None => lib_name,
        }))
    }

    /// Path dependencies, normal and build, including target-specific ones and those
    /// inherited from the workspace.
    fn dependencies(&mut self, dir: &str, cargo: &Table) -> Result<Vec<String>> {
        let targets = table(cargo, "target")
            .into_iter()
            .flat_map(|targets| targets.values());
        let scopes = std::iter::once(cargo).chain(targets.filter_map(Value::as_table));
        let specs: Vec<(String, Table)> = scopes
            .flat_map(|scope| DEPENDENCY_TABLES.iter().filter_map(|key| table(scope, key)))
            .flat_map(|dependencies| dependencies.iter())
            .filter_map(|(name, spec)| Some((name.clone(), spec.as_table()?.clone())))
            .collect();
        let mut found = Vec::new();
        for (name, spec) in specs {
            if let Some(path) = text(&spec, "path") {
                found.extend(normalize(&join(dir, path)));
            } else if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
                found.extend(self.workspace_dependency(dir, &name)?);
            }
        }
        Ok(found)
    }

    fn workspace_dependency(&mut self, dir: &str, name: &str) -> Result<Option<String>> {
        for ancestor in ancestors(dir) {
            let Some(manifest) = self.parsed(&join(ancestor, "Cargo.toml"))? else {
                continue;
            };
            if let Some(workspace) = table(&manifest, "workspace") {
                let spec = table(workspace, "dependencies").and_then(|deps| table(deps, name));
                let path = spec.and_then(|spec| text(spec, "path"));
                return Ok(path.and_then(|path| normalize(&join(ancestor, path))));
            }
        }
        Ok(None)
    }
}

impl Crates {
    pub(super) fn discover(host: &Path, files: &[IndexedFile]) -> Result<Self> {
        let mut manifests = Manifests {
            host,
            read: BTreeMap::new(),
            hashes: BTreeMap::new(),
        };
        let mut owner = BTreeMap::new();
        for file in files.iter().filter(|file| file.language == "rust") {
            owner.insert(file.path.clone(), manifests.package_of(&file.path)?);
        }
        let indexed: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
        let mut crates = BTreeMap::new();
        for dir in owner.values().flatten().collect::<BTreeSet<_>>() {
            crates.insert(dir.clone(), manifests.read_crate(dir, &indexed)?);
        }
        let digest = Sha256::digest(serde_json::to_vec(&manifests.hashes)?);
        Ok(Self {
            crates,
            owner,
            digest: format!("{digest:x}"),
        })
    }

    /// Whether `path` belongs to a closed crate, where an untraced call hides nothing.
    pub(super) fn linked(&self, path: &str) -> bool {
        let owner = self.owner.get(path).and_then(Option::as_ref);
        owner.is_some_and(|dir| self.crates[dir].closed)
    }

    /// Roots of the crates that build the module an import names, or a parent of it.
    pub(super) fn module_roots<'a>(
        &'a self,
        names: &'a [&'a str],
    ) -> impl Iterator<Item = &'a str> {
        self.crates.values().filter_map(move |krate| {
            let module = krate.module.as_deref()?;
            let named = names.iter().any(|name| {
                name.strip_prefix(module)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            });
            named.then_some(krate.root.as_deref()?)
        })
    }

    /// Write each root's edges to its files and path dependencies, then add to `flagged`
    /// every Rust file outside a crate, every file of a crate with no root, and the root
    /// of each crate that is not closed or that nothing outside it imports.
    pub(super) fn write_edges(
        &self,
        tx: &rusqlite::Transaction<'_>,
        flagged: &mut BTreeSet<String>,
    ) -> Result<()> {
        for (path, dir) in &self.owner {
            match dir.as_ref().and_then(|dir| self.crates[dir].root.as_ref()) {
                Some(root) => insert_import_edge(tx, root, path)?,
                None => {
                    flagged.insert(path.clone());
                }
            }
        }
        for krate in self.crates.values() {
            let Some(root) = &krate.root else { continue };
            for dependency in &krate.dependencies {
                if let Some(target) = self
                    .crates
                    .get(dependency)
                    .and_then(|dep| dep.root.as_ref())
                {
                    insert_import_edge(tx, root, target)?;
                }
            }
        }
        let mut importers = tx.prepare(
            "SELECT source.file_path FROM edges JOIN nodes AS source \
             ON source.qualified_name=edges.source_qualified WHERE edges.target_qualified=?1",
        )?;
        for (dir, krate) in &self.crates {
            let Some(root) = &krate.root else { continue };
            let sources = importers
                .query_map([format!("{root}::<module>")], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let outside = sources
                .iter()
                .any(|source| self.owner.get(source).and_then(Option::as_ref) != Some(dir));
            if !krate.closed || !outside {
                flagged.insert(root.clone());
            }
        }
        Ok(())
    }
}
