//! Explicit standalone Mull subjects; no project build instructions are inferred.
use super::*;

pub fn validate_mull_path(path: &str) -> Result<(), String> {
    crate::mutation_snapshot_inputs::validate_path(path)?;
    if path.starts_with('-')
        || path
            .bytes()
            .any(|b| matches!(b, b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b':' | b'\\'))
    {
        return Err("confined Mull requires a literal translation-unit path".into());
    }
    family(path)
        .ok_or_else(|| "confined Mull requires a C or C++ translation unit".into())
        .map(|_| ())
}
fn family(path: &str) -> Option<bool> {
    if path.ends_with(".c") {
        Some(false)
    } else if [".cc", ".cpp", ".cxx"]
        .iter()
        .any(|ext| path.ends_with(ext))
    {
        Some(true)
    } else {
        None
    }
}
fn pin(root: &Path, relative: &str) -> Result<String, String> {
    validate_mull_path(relative)?;
    let path = root.join(relative);
    let meta =
        fs::symlink_metadata(&path).map_err(|e| format!("cannot inspect {relative}: {e}"))?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() > 32 * 1024 * 1024
        || path.canonicalize().map_err(|e| e.to_string())? != path
    {
        return Err(format!(
            "confined Mull input is unsafe or too large: {relative}"
        ));
    }
    let bytes = fs::read(&path).map_err(|e| format!("cannot read {relative}: {e}"))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("confined Mull input exceeds byte limit".into());
    }
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}
pub(super) fn plan(
    request: &PlanRequest,
    covered: &BTreeSet<String>,
) -> Result<(Vec<Value>, Vec<String>), String> {
    let selected = request
        .only_sources
        .as_ref()
        .ok_or("confined Mull requires explicit source scope")?;
    let scope: BTreeSet<_> = selected.iter().cloned().collect();
    if scope.is_empty()
        || scope.len() != selected.len()
        || scope.len() != request.extra_tests.len()
        || !scope
            .iter()
            .all(|source| request.extra_tests.contains_key(source))
    {
        return Err("confined Mull requires one explicit test pair per scoped source".into());
    }
    let root = Path::new(&request.repo_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let sources: Vec<_> = scope.iter().cloned().collect();
    let slugs = unique_slugs(&sources);
    let mut manifests = Vec::new();
    let mut already = Vec::new();
    for source in &sources {
        let drivers = &request.extra_tests[source];
        if drivers.len() != 1
            || source == &drivers[0]
            || family(source).is_none()
            || family(source) != family(&drivers[0])
        {
            return Err(
                "confined Mull requires one distinct C/C++ driver of the same language".into(),
            );
        }
        let source_hash = pin(&root, source)?;
        let test_hash = pin(&root, &drivers[0])?;
        if covered.contains(source) {
            already.push(source.clone());
            continue;
        }
        let id = campaign_id(&request.owner, &slugs[source], "mull", &request.day);
        manifests.push(json!({"schema_version":1,"campaign_id":id,"title":format!("Generated Mull mutants for {source}"),
            "language":"cpp","mutation_engine":"mull", "generator":{"source":[source],"exclude":[],"operators":[],"options":{"execution_profile":"mull-confined-v1"},"seed":0,"jobs":1,"run_timeout_seconds":request.run_timeout_seconds},
            "test_argv":["forge-mull-test"],"environment":{},"source_sha256":{(source):source_hash},"test_sha256":{(&drivers[0]):test_hash},
            "survivor_baseline":[],"survivor_baseline_recorded":false,"survivor_baseline_note":SURVIVOR_BASELINE_NOTE}));
    }
    Ok((manifests, already))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Tree(PathBuf);
    impl Tree {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "mull-plan-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn request(root: &Path) -> PlanRequest {
        serde_json::from_value(json!({"language":"cpp","repo_root":root,"owner":"lane","day":"20261106","jobs":1,"mull_confined":true,"run_timeout_seconds":30,"campaigns_root":"campaigns","only_sources":["src/a.cpp"],"extra_tests":{"src/a.cpp":["tests/a.cc"]}})).unwrap()
    }
    #[test]
    fn generated_pair_is_explicit_and_fully_pinned() {
        let root = Tree::new();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("tests")).unwrap();
        fs::write(root.path().join("src/a.cpp"), "int value(){return 1;}").unwrap();
        fs::write(root.path().join("tests/a.cc"), "int main(){return 0;}").unwrap();
        let request = request(root.path());
        let result = compute_plan(&request).unwrap();
        let row = &result["manifests"][0];
        assert_eq!(
            row["generator"]["options"],
            json!({"execution_profile":"mull-confined-v1"})
        );
        assert_eq!(row["generator"]["source"], json!(["src/a.cpp"]));
        assert_eq!(row["test_argv"], json!(["forge-mull-test"]));
        assert_eq!(
            row["source_sha256"]["src/a.cpp"],
            format!("{:x}", Sha256::digest(b"int value(){return 1;}"))
        );
        assert_eq!(
            row["test_sha256"]["tests/a.cc"],
            format!("{:x}", Sha256::digest(b"int main(){return 0;}"))
        );
    }
    #[test]
    fn scope_driver_and_worker_ambiguity_refuse() {
        let root = Tree::new();
        let mut value = json!({"language":"cpp","repo_root":root.path(),"owner":"lane","day":"20261106","jobs":1,"mull_confined":true,"run_timeout_seconds":30,"campaigns_root":"campaigns","only_sources":["src/a.cpp"],"extra_tests":{"src/a.cpp":["tests/a.cc"]}});
        for (key, bad) in [
            ("jobs", json!(2)),
            ("language", json!("rust")),
            ("mull_confined", json!(false)),
            ("only_sources", json!(null)),
            ("extra_tests", json!({"src/a.cpp":["test.c"]})),
        ] {
            let original = value[key].clone();
            value[key] = bad;
            let request: PlanRequest = serde_json::from_value(value.clone()).unwrap();
            assert!(compute_plan(&request).is_err(), "{key}");
            value[key] = original;
        }
    }
}
