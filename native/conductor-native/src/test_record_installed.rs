//! Installed distributions: which one owns a file under a site directory, which
//! others it requires, and which names are installed. A test depends on the
//! distributions whose files it ran or read rather than on the whole environment,
//! so a dependency bump reruns only the tests that load what changed.

use crate::test_record_paths::{lexical, Place};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// PEP 503: case is ignored and a run of `-`, `_` and `.` is one separator.
fn normalized(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            separator = true;
            continue;
        }
        if separator && !out.is_empty() {
            out.push('-');
        }
        separator = false;
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// The project name in a `<name>-<version>.dist-info` or `.egg-info` directory name.
fn project(label: &str) -> Option<String> {
    let stem = label
        .strip_suffix(".dist-info")
        .or_else(|| label.strip_suffix(".egg-info"))?;
    Some(normalized(
        stem.split_once('-').map_or(stem, |(name, _)| name),
    ))
}

/// The path field of a RECORD row, a CSV field quoted when the path holds a comma.
fn record_path(row: &str) -> Option<String> {
    let Some(rest) = row.strip_prefix('"') else {
        return row.split(',').next().map(str::to_owned);
    };
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            out.push(ch);
        } else if chars.peek() == Some(&'"') {
            chars.next();
            out.push('"');
        } else {
            return Some(out);
        }
    }
    None
}

/// The projects a METADATA header requires outside any extra. An extra's requirement
/// is skipped: a test that runs the optional code depends on it directly.
fn requirements(metadata: &str) -> Vec<String> {
    metadata
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("Requires-Dist:"))
        .filter(|req| {
            !req.split_once(';')
                .is_some_and(|(_, marker)| marker.contains("extra"))
        })
        .filter_map(|req| {
            let req = req.trim();
            let end = req
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
                .unwrap_or(req.len());
            (end > 0).then(|| normalized(&req[..end]))
        })
        .collect()
}

/// Distribution metadata directories in `site`, by directory name.
fn metadata_dirs(site: &Path) -> std::io::Result<Vec<String>> {
    let mut labels = Vec::new();
    let Ok(entries) = std::fs::read_dir(site) else {
        return Ok(labels);
    };
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if project(&name).is_some() {
            labels.push(name);
        }
    }
    labels.sort();
    Ok(labels)
}

/// The site directories of the session's interpreter and what is installed in them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Installed {
    /// Each site directory as given and canonical: `co_filename` uses the spelling
    /// on `sys.path`, which a symlinked install does not canonicalize back into.
    sites: Vec<(PathBuf, PathBuf)>,
    /// Site-relative installed file to the metadata directory naming its distribution;
    /// the first site directory wins, as on `sys.path`.
    owners: HashMap<String, String>,
    /// Normalized project name to its metadata directory.
    projects: HashMap<String, String>,
}

impl Installed {
    /// Reads every RECORD once. Egg-info distributions list no owned files, so their
    /// files are recorded as plain host files.
    pub(crate) fn new(site_dirs: &[String]) -> std::io::Result<Self> {
        let mut installed = Self::default();
        for site in site_dirs {
            let given = lexical(Path::new(site));
            let canonical = std::fs::canonicalize(&given).unwrap_or_else(|_| given.clone());
            for label in metadata_dirs(&canonical)? {
                if let Some(name) = project(&label) {
                    installed
                        .projects
                        .entry(name)
                        .or_insert_with(|| label.clone());
                }
                let Ok(record) = std::fs::read(canonical.join(&label).join("RECORD")) else {
                    continue;
                };
                for row in String::from_utf8_lossy(&record).lines() {
                    let Some(path) = record_path(row) else {
                        continue;
                    };
                    if !path.is_empty() && !path.starts_with("../") && !path.starts_with('/') {
                        installed
                            .owners
                            .entry(path)
                            .or_insert_with(|| label.clone());
                    }
                }
            }
            installed.sites.push((given, canonical));
        }
        Ok(installed)
    }

    /// The installed project names, without versions: an install or removal changes
    /// them, a version bump does not.
    pub(crate) fn names(site_dirs: &[String]) -> std::io::Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        for site in site_dirs {
            names.extend(
                metadata_dirs(Path::new(site))?
                    .iter()
                    .filter_map(|label| project(label)),
            );
        }
        Ok(names)
    }

    /// The place of a path under a site directory: its distribution, or the host path
    /// itself when no RECORD lists it. A site directory itself is no dependency: its
    /// entries are the versioned metadata directories, which every bump renames, and
    /// the environment key holds the installed names. `None` when the path is under
    /// no site directory.
    pub(crate) fn classify(
        &self,
        joined: &Path,
        canonical: &Path,
    ) -> Option<Option<(Place, PathBuf)>> {
        let spelled = lexical(joined);
        let rel = self.sites.iter().find_map(|(given, real)| {
            spelled
                .strip_prefix(given)
                .or_else(|_| spelled.strip_prefix(real))
                .or_else(|_| canonical.strip_prefix(real))
                .ok()
        })?;
        if rel.as_os_str().is_empty() {
            return Some(None);
        }
        let owner = rel.to_str().and_then(|rel| self.owners.get(rel));
        Some(Some(match owner {
            Some(label) => (Place::Dist(label.clone()), canonical.to_path_buf()),
            None => {
                let shown = canonical.to_string_lossy().into_owned();
                (Place::Abs(shown), canonical.to_path_buf())
            }
        }))
    }

    /// The metadata directory a recorded label names in this environment; absent when
    /// the distribution is not installed here, or under another version.
    pub(crate) fn dist_dir(&self, label: &str) -> PathBuf {
        let mut dirs = self.sites.iter().map(|(_, real)| real.join(label));
        let first = dirs.next().unwrap_or_else(|| PathBuf::from(label));
        std::iter::once(first.clone())
            .chain(dirs)
            .find(|dir| dir.is_dir())
            .unwrap_or(first)
    }

    /// The installed distributions `label` requires directly, by metadata directory.
    pub(crate) fn requires(&self, label: &str) -> Vec<String> {
        let metadata = if label.ends_with(".egg-info") {
            "PKG-INFO"
        } else {
            "METADATA"
        };
        let Ok(text) = std::fs::read_to_string(self.dist_dir(label).join(metadata)) else {
            return Vec::new();
        };
        requirements(&text)
            .iter()
            .filter_map(|name| self.projects.get(name).cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(site: &Path, label: &str, record: &str, metadata: &str) {
        let dir = site.join(label);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("RECORD"), record).unwrap();
        std::fs::write(dir.join("METADATA"), metadata).unwrap();
    }

    #[test]
    fn names_normalize_and_requirements_skip_extras() {
        assert_eq!(normalized("Foo__Bar.baz-"), "foo-bar-baz");
        assert_eq!(
            project("typing_extensions-4.12.dist-info").unwrap(),
            "typing-extensions"
        );
        assert_eq!(project("legacy-1.0.egg-info").unwrap(), "legacy");
        assert_eq!(project("pkg"), None);
        assert_eq!(record_path("\"a,b.py\",sha256=x,1").unwrap(), "a,b.py");
        assert_eq!(
            record_path("\"say \"\"hi\"\".py\",,").unwrap(),
            "say \"hi\".py"
        );
        let metadata = "Name: x\nRequires-Dist: Typing_Extensions>=4\n\
            Requires-Dist: numpy (>=1.26) ; python_version >= \"3.10\"\n\
            Requires-Dist: pytest-cov>=5.0; extra == 'mutation'\n\nRequires-Dist: body\n";
        assert_eq!(requirements(metadata), ["typing-extensions", "numpy"]);
    }

    #[test]
    fn site_files_belong_to_the_distribution_whose_record_lists_them() {
        let root =
            std::env::temp_dir().join(format!("test-record-installed-{}", std::process::id()));
        let site = root.join("site-packages");
        dist(
            &site,
            "alpha-1.0.dist-info",
            "alpha/__init__.py,sha256=a,1\n\"alpha/odd,name.py\",,\n../../../bin/alpha,,\n",
            "Name: alpha\nRequires-Dist: beta\nRequires-Dist: missing\n",
        );
        dist(
            &site,
            "beta-2.0.dist-info",
            "beta.py,sha256=b,1\n",
            "Name: beta\n",
        );
        let sites = [site.to_str().unwrap().to_owned()];
        let installed = Installed::new(&sites).unwrap();
        let place = |rel: &str| {
            let path = site.join(rel);
            installed
                .classify(&path, &path)
                .flatten()
                .map(|(place, _)| place)
        };
        let alpha = Place::Dist("alpha-1.0.dist-info".to_owned());
        assert_eq!(place("alpha/__init__.py"), Some(alpha.clone()));
        assert_eq!(place("alpha/odd,name.py"), Some(alpha));
        let stray = site.join("_virtualenv.py");
        assert_eq!(
            place("_virtualenv.py"),
            Some(Place::Abs(stray.to_string_lossy().into_owned()))
        );
        assert_eq!(
            installed.classify(&root.join("x.py"), &root.join("x.py")),
            None
        );
        assert_eq!(installed.classify(&site, &site), Some(None));
        // An unowned `alpha/` spelling through `..` still resolves under the site.
        let dotted = site.join("beta/../alpha/__init__.py");
        assert!(matches!(
            installed.classify(&dotted, &dotted),
            Some(Some((Place::Dist(_), _)))
        ));
        assert_eq!(
            installed.requires("alpha-1.0.dist-info"),
            ["beta-2.0.dist-info"]
        );
        assert_eq!(
            installed.dist_dir("alpha-1.0.dist-info"),
            site.join("alpha-1.0.dist-info")
        );
        assert!(!installed.dist_dir("alpha-2.0.dist-info").exists());
        assert_eq!(
            Installed::names(&sites)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
