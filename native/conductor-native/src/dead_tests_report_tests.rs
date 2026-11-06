use super::*;
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Default)]
struct Fixture {
    files: BTreeMap<String, Vec<u8>>,
    untracked: BTreeSet<String>,
    lookups: Vec<String>,
    fail_lookup: bool,
}
impl Inputs for Fixture {
    fn read(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing captured source"))
    }
    fn is_file(&mut self, path: &str) -> anyhow::Result<bool> {
        self.lookups.push(path.into());
        anyhow::ensure!(!self.fail_lookup, "lookup failure");
        Ok(self.untracked.contains(path))
    }
}
fn add(fixture: &mut Fixture, path: &str, text: &str) {
    fixture.files.insert(path.into(), text.as_bytes().to_vec());
}
#[test]
fn classification_retains_precedence_guarded_and_notes_distinctions() {
    let mut input = Fixture::default();
    add(&mut input, "pkg/__init__.py", "");
    add(&mut input, "pkg/subject.py", "import pkg.local\n");
    add(
        &mut input,
        "pkg/lazy.py",
        "def later():\n    import pkg.retired\n",
    );
    add(&mut input, "pkg/configured.py", "VALUE = 1\n");
    add(&mut input, "pkg/dynamic.py", "VALUE = 1\n");
    add(&mut input, "pkg/loader.py", "VALUE = 'dynamic.py'\n");
    add(
        &mut input,
        "tests/test_broken.py",
        "import pkg.subject\nimport pkg.missing\n",
    );
    add(&mut input, "tests/test_subject.py", "import pkg.subject\n");
    add(
        &mut input,
        "tests/test_configured.py",
        "import pkg.configured\n",
    );
    input.untracked.insert("pkg/local.py".into());
    let tracked: Vec<_> = input.files.keys().cloned().collect();
    let dates = BTreeMap::from([("tests/test_broken.py".to_owned(), "2026-11-06".to_owned())]);
    let analysis = Analysis::from_captured(Path::new("/repo"), &tracked, &mut input).unwrap();
    let report = serde_json::to_value(
        analysis
            .report("pkg.configured", "pkg.subject", &dates)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(report["tests_scanned"], 3);
    assert_eq!(
        report["broken"],
        json!([{"test":"tests/test_broken.py","missing":["pkg.missing"],"last_commit":"2026-11-06"}])
    );
    assert_eq!(
        report["depends_on_untracked"],
        json!([{"test":"tests/test_subject.py","untracked":["pkg/local.py"],"last_commit":null}])
    );
    assert_eq!(
        report["orphan_target"],
        json!([{"test":"tests/test_subject.py","targets":["pkg/subject.py"],"notes_only":["pkg/subject.py"],"last_commit":null}])
    );
    assert_eq!(
        report["stale_imports"],
        json!([{"module":"pkg/lazy.py","missing":["pkg.retired"],"last_commit":null}])
    );
    assert_eq!(
        report["untracked_importers"]["pkg/local.py"],
        json!(["pkg/subject.py"])
    );
    let unreferenced = report["unreferenced_sources"].as_array().unwrap();
    assert!(!unreferenced
        .iter()
        .any(|row| row["path"] == "pkg/configured.py" || row["path"] == "pkg/dynamic.py"));
    assert!(input.lookups.iter().any(|path| path == "pkg/local.py"));
    assert!(input
        .lookups
        .iter()
        .any(|path| path.ends_with("pkg/missing.py")));
}
#[test]
fn captured_parser_is_lossy_and_observation_errors_are_not_absence() {
    let mut input = Fixture::default();
    input
        .files
        .insert("pkg/a.py".into(), b"# \xff\nVALUE = 1\n".to_vec());
    let tracked = vec!["pkg/a.py".to_owned()];
    let analysis = Analysis::from_captured(Path::new("/repo"), &tracked, &mut input).unwrap();
    assert_eq!(
        analysis
            .report("", "", &BTreeMap::new())
            .unwrap()
            .modules_scanned,
        1
    );
    input.files.clear();
    assert!(matches!(
        Analysis::from_captured(Path::new("/repo"), &tracked, &mut input),
        Err(ScanError::Read { .. })
    ));
    add(&mut input, "pkg/a.py", "import pkg.missing\n");
    input.fail_lookup = true;
    assert!(matches!(
        Analysis::from_captured(Path::new("/repo"), &tracked, &mut input),
        Err(ScanError::Lookup { .. })
    ));
    add(&mut input, "pkg/a.py", "if:\n");
    assert!(matches!(
        Analysis::from_captured(Path::new("/repo"), &tracked, &mut input),
        Err(ScanError::Parse { .. })
    ));
}
