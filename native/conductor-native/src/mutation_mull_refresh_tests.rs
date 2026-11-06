use super::*;
use crate::mutation_plan::{compute_plan, PlanRequest};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mull-refresh-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn generated(ext: &str) -> Value {
    let tree = Tree::new();
    fs::create_dir(tree.0.join("src")).unwrap();
    fs::create_dir(tree.0.join("tests")).unwrap();
    let source = format!("src/a.{ext}");
    let driver = format!("tests/driver.{ext}");
    fs::write(tree.0.join(&source), "int answer(void){return 1;}\n").unwrap();
    fs::write(tree.0.join(&driver), "int main(void){return 0;}\n").unwrap();
    let request: PlanRequest = serde_json::from_value(json!({"language":"cpp","repo_root":tree.0,"owner":"lane","day":"20261106","jobs":1,"mull_confined":true,"run_timeout_seconds":30,"campaigns_root":"campaigns","only_sources":[source],"extra_tests":{(source):[driver]}})).unwrap();
    compute_plan(&request).unwrap()["manifests"][0].clone()
}
#[test]
fn refresh_preserves_all_nonpin_fields_for_recorded_and_unrecorded_baselines() {
    for ext in ["c", "cpp"] {
        for recorded in [false, true] {
            let mut original = generated(ext);
            original["opaque_field"] = json!({"value":[1,2,3]});
            if recorded {
                original["survivor_baseline"] = json!(["m-1", "m-2"]);
                original["survivor_baseline_recorded"] = json!(true);
                original["survivor_baseline_recorded_at"] = json!("2026-11-06T00:00:00Z");
                original
                    .as_object_mut()
                    .unwrap()
                    .remove("survivor_baseline_note");
            } else {
                original
                    .as_object_mut()
                    .unwrap()
                    .remove("survivor_baseline_recorded");
            }
            let (source, driver) = inputs(&original).unwrap();
            let refreshed = refresh(&original, &"a".repeat(64), &"b".repeat(64)).unwrap();
            let mut expected = original.clone();
            expected["source_sha256"][&source] = json!("a".repeat(64));
            expected["test_sha256"][&driver] = json!("b".repeat(64));
            assert_eq!(refreshed, expected);
            assert_eq!(inputs(&refreshed).unwrap(), (source, driver));
        }
    }
}
#[test]
fn malformed_or_unsafe_contracts_and_pins_are_refused() {
    let good = generated("cpp");
    for case in [
        "profile",
        "options",
        "argv",
        "language",
        "source",
        "mixed",
        "id",
        "oldhash",
        "duplicates",
        "recorded",
        "contradiction",
        "note",
    ] {
        let mut bad = good.clone();
        match case {
            "profile" => bad["generator"]["options"]["execution_profile"] = json!("other"),
            "options" => bad["generator"]["options"]["extra"] = json!(true),
            "argv" => bad["test_argv"] = json!(["sh"]),
            "language" => bad["language"] = json!("rust"),
            "source" => {
                bad["generator"]["source"] = json!(["../a.cpp"]);
                bad["source_sha256"] = json!({"../a.cpp":"a".repeat(64)});
            }
            "mixed" => bad["test_sha256"] = json!({"tests/driver.c":"a".repeat(64)}),
            "id" => bad["campaign_id"] = json!("../unsafe"),
            "oldhash" => bad["source_sha256"]["src/a.cpp"] = json!("bad"),
            "duplicates" => bad["survivor_baseline"] = json!(["same", "same"]),
            "recorded" => bad["survivor_baseline_recorded"] = json!("yes"),
            "contradiction" => {
                bad["survivor_baseline_recorded"] = json!(false);
                bad["survivor_baseline"] = json!(["id"]);
            }
            "note" => bad["survivor_baseline_note"] = json!(false),
            _ => unreachable!(),
        }
        assert!(inputs(&bad).is_err(), "{case}");
    }
    for bad in ["", "abcd", &"A".repeat(64)] {
        assert!(refresh(&good, bad, &"b".repeat(64)).is_err());
        assert!(refresh(&good, &"a".repeat(64), bad).is_err());
    }
}
