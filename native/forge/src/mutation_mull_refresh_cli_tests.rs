use super::*;
use conductor_native::mutation_plan::{compute_plan, PlanRequest};
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    campaign: PathBuf,
    source: PathBuf,
    driver: PathBuf,
    fragment: PathBuf,
}

impl Fixture {
    fn new(ext: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("tests")).unwrap();
        let source = PathBuf::from(format!("src/subject.{ext}"));
        let driver = PathBuf::from(format!("tests/driver.{ext}"));
        fs::write(
            root.path().join(&source),
            "int add(int a,int b){return a+b;}\n",
        )
        .unwrap();
        fs::write(root.path().join(&driver), "int main(void){return 0;}\n").unwrap();
        let request: PlanRequest = serde_json::from_value(json!({
            "language":"cpp", "repo_root":root.path(), "owner":"refresh", "day":"20261106",
            "jobs":1, "mull_confined":true, "run_timeout_seconds":30,
            "campaigns_root":"campaigns", "only_sources":[source],
            "extra_tests":{(source.to_str().unwrap()):[driver]}
        }))
        .unwrap();
        let plan = compute_plan(&request).unwrap();
        let written = crate::mutation_write::write_manifests(
            plan["manifests"].as_array().unwrap(),
            root.path(),
            "campaigns/registry.json",
            "campaigns",
            false,
        )
        .unwrap();
        fs::set_permissions(
            root.path().join("campaigns"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        Self {
            root,
            campaign: PathBuf::from(&written[0]),
            source,
            driver,
            fragment: PathBuf::from(&written[1]),
        }
    }
    fn path(&self, relative: &Path) -> PathBuf {
        self.root.path().join(relative)
    }
    fn prepare(&self) -> Prepared {
        prepare(self.root.path(), &self.campaign).unwrap()
    }
    fn change(&self) {
        fs::write(
            self.path(&self.source),
            "int add(int a,int b){return a-b;}\n",
        )
        .unwrap();
        fs::write(self.path(&self.driver), "int main(void){return 1;}\n").unwrap();
    }
}

#[test]
fn preview_and_apply_preserve_scope_and_registration_and_current_apply_is_noop() {
    for ext in ["c", "cpp"] {
        let f = Fixture::new(ext);
        let old = fs::read(f.path(&f.campaign)).unwrap();
        let fragment = fs::read(f.path(&f.fragment)).unwrap();
        f.change();
        let preview = f.prepare().finish(false, true).unwrap();
        assert_eq!(preview["status"], "PREVIEW");
        assert_eq!(preview["changed"], true);
        assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), old);
        let mut old_value: Value = serde_json::from_slice(&old).unwrap();
        old_value["source_sha256"] = preview["manifest"]["source_sha256"].clone();
        old_value["test_sha256"] = preview["manifest"]["test_sha256"].clone();
        assert_eq!(preview["manifest"], old_value);
        let source = fs::read(f.path(&f.source)).unwrap();
        let driver = fs::read(f.path(&f.driver)).unwrap();
        let applied = f.prepare().finish(true, false).unwrap();
        assert_eq!(applied["status"], "APPLIED");
        assert!(applied.get("manifest").is_none());
        let new_bytes = fs::read(f.path(&f.campaign)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&new_bytes).unwrap(),
            old_value
        );
        assert_eq!(sha256_hex(&new_bytes), preview["proposed_manifest_sha256"]);
        assert_ne!(
            preview["old_manifest_sha256"],
            preview["proposed_manifest_sha256"]
        );
        let inode = fs::metadata(f.path(&f.campaign)).unwrap().ino();
        assert_eq!(
            f.prepare().finish(true, false).unwrap()["status"],
            "UNCHANGED"
        );
        assert_eq!(fs::metadata(f.path(&f.campaign)).unwrap().ino(), inode);
        assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), new_bytes);
        assert_eq!(fs::read(f.path(&f.fragment)).unwrap(), fragment);
        assert_eq!(fs::read(f.path(&f.source)).unwrap(), source);
        assert_eq!(fs::read(f.path(&f.driver)).unwrap(), driver);
    }
}

#[test]
fn competing_refresh_cannot_overwrite_a_completed_refresh() {
    let f = Fixture::new("cpp");
    f.change();
    let stale = f.prepare();
    f.prepare().finish(true, false).unwrap();
    let current = fs::read(f.path(&f.campaign)).unwrap();
    assert!(stale.finish(true, false).is_err());
    assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), current);
}

#[test]
fn late_manifest_or_input_replacement_refuses_and_removes_only_staging() {
    for kind in ["manifest", "source", "driver"] {
        let f = Fixture::new("cpp");
        f.change();
        let prepared = f.prepare();
        let before = fs::read(f.path(&f.campaign)).unwrap();
        let target = match kind {
            "manifest" => &f.campaign,
            "source" => &f.source,
            _ => &f.driver,
        };
        let result =
            candidate_report_store::publish_checked(&prepared.destination, &prepared.bytes, || {
                let replacement = f.root.path().join("replacement");
                fs::write(&replacement, b"new unrelated content")?;
                fs::rename(replacement, f.path(target))?;
                prepared.verify()
            });
        assert!(result.is_err());
        let expected = if kind == "manifest" {
            b"new unrelated content".to_vec()
        } else {
            before
        };
        assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), expected);
        assert!(fs::read_dir(f.path(&f.campaign).parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".report-pending")));
    }
}

#[test]
fn malformed_json_and_profile_never_publish() {
    let f = Fixture::new("cpp");
    let valid = fs::read_to_string(f.path(&f.campaign)).unwrap();
    let duplicate = valid.replacen('{', "{\"schema_version\":1,", 1);
    // A permissive parser still yields the original valid profile: only strict
    // duplicate-key rejection can distinguish this case from the valid manifest.
    assert_eq!(
        serde_json::from_str::<Value>(&duplicate).unwrap(),
        serde_json::from_str::<Value>(&valid).unwrap()
    );
    for bytes in [duplicate.as_bytes(), b"{}"] {
        fs::write(f.path(&f.campaign), bytes).unwrap();
        assert!(prepare(f.root.path(), &f.campaign).is_err());
        assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), bytes);
    }
}

#[test]
fn paths_and_unsafe_input_aliases_are_refused() {
    for relative in [
        "../outside.json",
        ".git/manifest.json",
        "campaigns/../manifest.json",
    ] {
        let f = Fixture::new("cpp");
        assert!(prepare(f.root.path(), Path::new(relative)).is_err());
    }
    for kind in ["symlink", "hardlink", "world-writable", "oversize"] {
        let f = Fixture::new("cpp");
        let source = f.path(&f.source);
        let original = fs::read(f.path(&f.campaign)).unwrap();
        match kind {
            "symlink" => {
                fs::remove_file(&source).unwrap();
                symlink(f.path(&f.driver), &source).unwrap();
            }
            "hardlink" => {
                fs::hard_link(&source, f.root.path().join("alias")).unwrap();
            }
            "world-writable" => {
                fs::set_permissions(&source, fs::Permissions::from_mode(0o666)).unwrap()
            }
            _ => fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .unwrap()
                .set_len(SOURCE_LIMIT as u64 + 1)
                .unwrap(),
        }
        assert!(prepare(f.root.path(), &f.campaign).is_err(), "{kind}");
        assert_eq!(fs::read(f.path(&f.campaign)).unwrap(), original);
    }
}

#[test]
fn manifest_aliases_and_parent_symlinks_are_refused() {
    let f = Fixture::new("cpp");
    let manifest = f.path(&f.campaign);
    let alias = f.root.path().join("manifest-alias.json");
    fs::hard_link(&manifest, &alias).unwrap();
    assert!(prepare(f.root.path(), &f.campaign).is_err());
    fs::remove_file(&alias).unwrap();
    symlink(&manifest, &alias).unwrap();
    assert!(prepare(f.root.path(), Path::new("manifest-alias.json")).is_err());
    symlink(
        f.root.path().join("campaigns"),
        f.root.path().join("campaign-link"),
    )
    .unwrap();
    let path = Path::new("campaign-link").join(f.campaign.file_name().unwrap());
    assert!(prepare(f.root.path(), &path).is_err());
}

#[test]
fn shared_manifest_or_parent_remains_previewable_but_changed_apply_refuses() {
    for parent in [false, true] {
        let fixture = Fixture::new("cpp");
        fixture.change();
        let manifest = fixture.path(&fixture.campaign);
        let path = if parent {
            manifest.parent().unwrap()
        } else {
            &manifest
        };
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if parent { 0o775 } else { 0o664 }),
        )
        .unwrap();
        let old = fs::read(&manifest).unwrap();
        let fragment = fs::read(fixture.path(&fixture.fragment)).unwrap();
        assert_eq!(
            fixture.prepare().finish(false, false).unwrap()["status"],
            "PREVIEW"
        );
        let error = format!("{:#}", fixture.prepare().finish(true, false).unwrap_err());
        assert!(error.contains("mutation"), "{error}");
        assert_eq!(fs::read(&manifest).unwrap(), old);
        assert_eq!(fs::read(fixture.path(&fixture.fragment)).unwrap(), fragment);
        assert!(!fs::read_dir(manifest.parent().unwrap()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".report-pending")));
    }
}

#[test]
fn unchanged_apply_is_read_only_even_for_shared_manifest_metadata() {
    let fixture = Fixture::new("cpp");
    let manifest = fixture.path(&fixture.campaign);
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o664)).unwrap();
    fs::set_permissions(
        manifest.parent().unwrap(),
        fs::Permissions::from_mode(0o775),
    )
    .unwrap();
    let old = fs::read(&manifest).unwrap();
    let inode = fs::metadata(&manifest).unwrap().ino();
    assert_eq!(
        fixture.prepare().finish(true, false).unwrap()["status"],
        "UNCHANGED"
    );
    assert_eq!(fs::read(&manifest).unwrap(), old);
    assert_eq!(fs::metadata(&manifest).unwrap().ino(), inode);
    assert_eq!(fs::metadata(&manifest).unwrap().mode() & 0o777, 0o664);
}

#[test]
fn mutation_permission_drift_after_capture_refuses_refresh() {
    let fixture = Fixture::new("cpp");
    fixture.change();
    let prepared = fixture.prepare();
    let manifest = fixture.path(&fixture.campaign);
    let old = fs::read(&manifest).unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(prepared.finish(true, false).is_err());
    assert_eq!(fs::read(&manifest).unwrap(), old);
}
