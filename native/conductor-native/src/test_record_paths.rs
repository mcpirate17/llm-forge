//! Where a path a test touched lives, and therefore how its identity is recorded.

use std::path::{Component, Path, PathBuf};

/// A dependency location. Repository files are recorded by git blob oid so a
/// candidate tree can be matched without reading it; files under `$HOME` and other
/// absolute paths are host state and are recorded by `stat` identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Place {
    Repo(String),
    Home(String),
    Abs(String),
}

impl Place {
    pub(crate) fn label(&self) -> String {
        match self {
            Place::Repo(rel) => rel.clone(),
            Place::Home(rel) => format!("~/{rel}"),
            Place::Abs(path) => path.clone(),
        }
    }
}

/// Classification roots, all canonical. Environment prefixes (the venv, the base
/// interpreter, site-packages) are covered by the environment key, so they are
/// checked before the repository root: a venv inside the repository is not source.
#[derive(Debug)]
pub(crate) struct Places {
    root: PathBuf,
    environment: Vec<PathBuf>,
    scratch: Vec<PathBuf>,
    home: Option<PathBuf>,
}

const PSEUDO_FILESYSTEMS: [&str; 3] = ["/proc", "/sys", "/dev"];

fn canonical(path: &str) -> PathBuf {
    let path = Path::new(path);
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical(path))
}

/// `.` and `..` removed without touching the filesystem.
pub(crate) fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn ignored_in_repo(rel: &str) -> bool {
    rel == ".git" || rel.starts_with(".git/")
}

/// Bytecode caches, in `__pycache__` or under `PYTHONPYCACHEPREFIX`, derive from
/// their source, which is recorded.
fn bytecode(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "pyc")
        || path
            .components()
            .any(|part| part.as_os_str() == "__pycache__")
}

impl Places {
    pub(crate) fn new(
        root: &str,
        environment: &[String],
        scratch: &[String],
        home: Option<&str>,
    ) -> Self {
        Self {
            root: canonical(root),
            environment: environment.iter().map(|p| canonical(p)).collect(),
            scratch: scratch.iter().map(|p| canonical(p)).collect(),
            home: home.map(canonical),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// `raw` is resolved against `cwd` when relative. `None` means the path is
    /// covered elsewhere (environment key) or is not a dependency at all.
    pub(crate) fn classify(&self, raw: &Path, cwd: &Path) -> Option<(Place, PathBuf)> {
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            cwd.join(raw)
        };
        let path = std::fs::canonicalize(&joined).unwrap_or_else(|_| lexical(&joined));
        if bytecode(&path)
            || self
                .environment
                .iter()
                .any(|prefix| path.starts_with(prefix))
        {
            return None;
        }
        if let Ok(rel) = path.strip_prefix(&self.root) {
            let rel = rel.to_str()?.to_owned();
            if rel.is_empty() || ignored_in_repo(&rel) {
                return None;
            }
            return Some((Place::Repo(rel), path));
        }
        if PSEUDO_FILESYSTEMS.iter().any(|fs| path.starts_with(fs))
            || self.scratch.iter().any(|prefix| path.starts_with(prefix))
        {
            return None;
        }
        if let Some(home) = &self.home {
            if let Ok(rel) = path.strip_prefix(home) {
                return Some((Place::Home(rel.to_str()?.to_owned()), path));
            }
        }
        Some((Place::Abs(path.to_str()?.to_owned()), path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(root: &Path) -> Places {
        let venv = root.join(".venv");
        Places::new(
            root.to_str().unwrap(),
            &[venv.to_str().unwrap().to_owned()],
            &["/nonexistent-scratch".to_owned()],
            Some("/home/someone"),
        )
    }

    #[test]
    fn repository_files_are_relative_and_the_venv_inside_it_is_not_source() {
        let root = std::env::temp_dir().join("test-record-paths");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let places = places(&root);
        let (place, _) = places.classify(Path::new("pkg/mod.py"), &root).unwrap();
        assert_eq!(place, Place::Repo("pkg/mod.py".to_owned()));
        assert!(places
            .classify(Path::new(".venv/lib/x.py"), &root)
            .is_none());
        assert!(places
            .classify(Path::new("pkg/__pycache__/mod.cpython-312.pyc"), &root)
            .is_none());
        assert!(places.classify(Path::new(".git/HEAD"), &root).is_none());
    }

    #[test]
    fn host_paths_split_into_home_absolute_and_ignored() {
        let root = std::env::temp_dir().join("test-record-paths-host");
        let places = places(&root);
        let cwd = Path::new("/");
        assert_eq!(
            places
                .classify(Path::new("/home/someone/.cache/x"), cwd)
                .unwrap()
                .0,
            Place::Home(".cache/x".to_owned())
        );
        assert_eq!(
            places
                .classify(Path::new("/opt/data/../data/x.bin"), cwd)
                .unwrap()
                .0,
            Place::Abs("/opt/data/x.bin".to_owned())
        );
        assert!(places
            .classify(Path::new("/proc/self/status"), cwd)
            .is_none());
        assert!(places
            .classify(Path::new("/nonexistent-scratch/run/x"), cwd)
            .is_none());
        assert!(places
            .classify(
                Path::new("/home/someone/pycache/tmp/case/pkg/mod.cpython-312.pyc"),
                cwd
            )
            .is_none());
    }
}
