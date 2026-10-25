//! Where a path a test touched lives, and therefore how its identity is recorded.

use crate::test_record_installed::Installed;
use std::path::{Component, Path, PathBuf};

/// A dependency location. Repository files are recorded by git blob oid so a
/// candidate tree can be matched without reading it; files under `$HOME` and other
/// absolute paths are host state and are recorded by `stat` identity. A file an
/// installed distribution owns is recorded as that distribution, by its metadata
/// directory name, which carries the version.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Place {
    Repo(String),
    Home(String),
    Abs(String),
    Dist(String),
}

impl Place {
    pub(crate) fn label(&self) -> String {
        match self {
            Place::Repo(rel) => rel.clone(),
            Place::Home(rel) => format!("~/{rel}"),
            Place::Abs(path) | Place::Dist(path) => path.clone(),
        }
    }
}

/// Classification roots, all canonical. Site directories are checked first: a file
/// there belongs to the distribution that installed it. The other environment
/// prefixes (the venv, the base interpreter) are covered by the environment key, so
/// they are checked before the repository root: a venv inside the repository is not
/// source.
#[derive(Clone, Debug)]
pub(crate) struct Places {
    root: PathBuf,
    environment: Vec<PathBuf>,
    scratch: Vec<PathBuf>,
    home: Option<PathBuf>,
    installed: Installed,
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
            installed: Installed::default(),
        }
    }

    pub(crate) fn with_installed(self, installed: Installed) -> Self {
        Self { installed, ..self }
    }

    pub(crate) fn installed(&self) -> &Installed {
        &self.installed
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// A distribution's place and the metadata directory its identity is read from.
    pub(crate) fn dist(&self, label: &str) -> (Place, PathBuf) {
        (
            Place::Dist(label.to_owned()),
            self.installed.dist_dir(label),
        )
    }

    /// The place a recorded label names here: `~/` is this session's home, so a
    /// record made under another home names a different file.
    pub(crate) fn resolve(&self, label: &str) -> Option<(Place, PathBuf)> {
        if let Some(rel) = label.strip_prefix("~/") {
            let home = self.home.as_ref()?;
            return Some((Place::Home(rel.to_owned()), home.join(rel)));
        }
        if label.starts_with('/') {
            return Some((Place::Abs(label.to_owned()), PathBuf::from(label)));
        }
        Some((Place::Repo(label.to_owned()), self.root.join(label)))
    }

    /// A test's file relative to the root, and its node id re-rooted on that file,
    /// so records from different checkouts and pytest rootdirs of one repository agree.
    pub(crate) fn rerooted(&self, nodeid: &str, path: &Path) -> (String, PathBuf) {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let rel = real
            .strip_prefix(&self.root)
            .map_or_else(|_| real.clone(), Path::to_path_buf);
        let shown = rel.to_string_lossy();
        let canonical = match nodeid.split_once("::") {
            Some((_, suffix)) => format!("{shown}::{suffix}"),
            None => shown.into_owned(),
        };
        (canonical, rel)
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
        if bytecode(&path) {
            return None;
        }
        if let Some(found) = self.installed.classify(&joined, &path) {
            return found;
        }
        if self
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
    fn site_files_are_their_distribution_and_the_rest_of_the_venv_is_ignored() {
        let root =
            std::env::temp_dir().join(format!("test-record-paths-site-{}", std::process::id()));
        let site = root.join(".venv/lib/python3.12/site-packages");
        let meta = site.join("alpha-1.0.dist-info");
        std::fs::create_dir_all(site.join("alpha")).unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("RECORD"), "alpha/__init__.py,,\n").unwrap();
        let sites = [site.to_str().unwrap().to_owned()];
        let places = places(&root).with_installed(Installed::new(&sites).unwrap());
        let (place, _) = places
            .classify(
                Path::new(".venv/lib/python3.12/site-packages/alpha/__init__.py"),
                &root,
            )
            .unwrap();
        assert_eq!(place, Place::Dist("alpha-1.0.dist-info".to_owned()));
        assert_eq!(places.dist("alpha-1.0.dist-info").1, meta);
        let stray = site.join("_virtualenv.py");
        assert_eq!(
            places.classify(&stray, &root).unwrap().0,
            Place::Abs(stray.to_str().unwrap().to_owned())
        );
        assert!(places.classify(&site, &root).is_none());
        assert!(places
            .classify(Path::new(".venv/bin/python"), &root)
            .is_none());
        assert!(places
            .classify(
                &site.join("alpha/__pycache__/__init__.cpython-312.pyc"),
                &root
            )
            .is_none());
        std::fs::remove_dir_all(&root).unwrap();
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
