//! Read-only mutation evidence and coverage reports over held live inputs.
use crate::{
    candidate_source::Source, mutation_evidence_legacy::Legacy, mutation_live_catalog,
    mutation_live_inputs::LiveInputs,
};
use anyhow::{Context, Result};
use conductor_native::{mutation_coverage as coverage, mutation_evidence as evidence};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub enum Selection {
    Explicit(Vec<String>),
    Changed(Option<String>),
    Coverage,
    Canary,
}
pub struct ReportRequest {
    pub repo_root: PathBuf,
    pub registry: Option<String>,
    pub selection: Selection,
    pub legacy_component_root: Option<PathBuf>,
    pub include_untracked: bool,
    pub native_runner_pin: Option<(PathBuf, String)>,
}
const SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    ".codex",
    ".grok",
    ".tox",
    "dist",
    "build",
    "worktrees",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".code-review-graph",
];
fn registry_path(capture: &mut LiveInputs, root: &Path, explicit: Option<&str>) -> Result<String> {
    let path = if let Some(path) = explicit {
        path.to_owned()
    } else if let Some(path) = std::env::var_os("CONDUCTOR_MUTATION_REGISTRY") {
        let path = path
            .into_string()
            .map_err(|_| anyhow::anyhow!("registry override must be UTF-8"))?;
        if path.trim().is_empty() {
            configured_registry(capture)?
        } else {
            path
        }
    } else {
        configured_registry(capture)?
    };
    let path = Path::new(&path);
    let path = if path.is_absolute() {
        path.strip_prefix(root)
            .context("registry must be inside repository")?
    } else {
        path
    };
    let path = path.to_str().context("registry path must be UTF-8")?;
    conductor_native::project_paths::relative(path, "mutation registry").map_err(anyhow::Error::msg)
}
fn configured_registry(capture: &mut LiveInputs) -> Result<String> {
    let config = capture.workspace_config()?;
    conductor_native::project_paths::mutation_registry_from_config(config.as_ref())
        .map_err(anyhow::Error::msg)
}
fn projection(mut result: Value, registry: &str, selection: &Selection) -> Result<(Value, u8)> {
    match selection {
        Selection::Explicit(_) => {
            let code = if result["status"] == "PASS" && coverage::evidence_exit_code(&result) != 5 {
                0
            } else {
                5
            };
            Ok((result, code))
        }
        Selection::Changed(_) => {
            result["schema_version"] = json!("llm.mutation-testing.changed-evidence.v2");
            result["enforcement"] = json!("changed_tests");
            let code = coverage::evidence_exit_code(&result) as u8;
            Ok((result, code))
        }
        Selection::Coverage | Selection::Canary => {
            let covered = result["evidence"]
                .as_array()
                .context("evidence missing")?
                .len();
            let missing = result["missing_evidence"]
                .as_array()
                .context("missing evidence missing")?
                .len();
            let report = json!({"schema_version":"llm.mutation-testing.coverage.v2","status":result["status"],"enforcement":"repository_inventory","registry":registry,"total_test_files":covered+missing,"covered_test_files":covered,"missing_test_files":missing,"evidence":result["evidence"],"missing_evidence":result["missing_evidence"],"rejection_counts":result["rejection_counts"],"malformed_receipts":result["malformed_receipts"]});
            if matches!(selection, Selection::Canary) {
                let report = coverage::canary_verdict(report).map_err(anyhow::Error::msg)?;
                let code = if report["canary"]["status"] == "PASS" {
                    0
                } else {
                    5
                };
                Ok((report, code))
            } else {
                let code =
                    if report["status"] == "PASS" && coverage::evidence_exit_code(&report) != 5 {
                        0
                    } else {
                        5
                    };
                Ok((report, code))
            }
        }
    }
}
fn capture_pin(
    request: &ReportRequest,
    stamp: Option<&crate::candidate_engine_identity::BuildStamp>,
    source: &Source,
    capture: &mut LiveInputs,
    fingerprint: Option<&str>,
) -> Result<Option<crate::candidate_native_runner_pin::NativeRunnerPin>> {
    let Some((path, sha256)) = &request.native_runner_pin else {
        return Ok(None);
    };
    anyhow::ensure!(
        fingerprint.is_none() && capture.native_inventory_absent()?,
        "installed runner requires complete native source inventory absence"
    );
    let stamp = stamp.context("native runner pin requires trusted CLI build identity")?;
    Ok(Some(
        crate::candidate_native_runner_pin::NativeRunnerPin::capture(
            path,
            sha256,
            &[
                source.root().to_path_buf(),
                source.common_dir().to_path_buf(),
            ],
            stamp,
        )?,
    ))
}
pub fn run_report(
    request: &ReportRequest,
    stamp: Option<&crate::candidate_engine_identity::BuildStamp>,
) -> Result<(Value, u8)> {
    let source = Source::open(&request.repo_root)?;
    let root = source.root();
    let mut capture = LiveInputs::open(root)?;
    let registry_path = registry_path(&mut capture, root, request.registry.as_deref())?;
    let registry = mutation_live_catalog::registry(&mut capture, &registry_path)?;
    let catalog = mutation_live_catalog::catalog(&mut capture, &registry)?;
    let paths = crate::mutation_evidence_git_inventory::select(
        &source,
        &mut capture,
        &request.selection,
        request.include_untracked,
        SKIP,
    )?;
    for path in &paths {
        capture.read(path)?;
    }
    let legacy = Legacy::capture(request.legacy_component_root.as_deref())?;
    let fingerprint = if catalog.has_native_receipts {
        capture.native_source_fingerprint()?
    } else {
        None
    };
    let pin = capture_pin(
        request,
        stamp,
        &source,
        &mut capture,
        fingerprint.as_deref(),
    )?;
    let installed = pin.as_ref().map(|pin| pin.identity());
    let supporting = crate::mutation_cargo_supporting::required_for(
        &catalog.contracts,
        &catalog.receipts,
        || Ok(fingerprint.clone()),
        installed,
    )
    .and_then(|required| {
        required
            .map(|paths| capture.supporting_inventory(&paths))
            .transpose()
    })
    .map_err(|error| format!("{error:#}"));
    let required = crate::mutation_cargo_supporting::workspace_required_for(
        &catalog.contracts,
        &catalog.receipts,
        || Ok(fingerprint.clone()),
        installed,
    )?;
    let workspace =
        crate::mutation_cargo_supporting::capture_workspace_profiles(required, |pins| {
            capture.workspace_inventory(pins)
        });
    let config = capture.workspace_config()?;
    let receipt_root = if workspace.is_empty() {
        None
    } else {
        Some(
            conductor_native::project_paths::mutation_receipt_root_from_config(config.as_ref())
                .map_err(anyhow::Error::msg)?,
        )
    };
    let bindings = evidence::WorkspaceBindings {
        identities: &workspace,
        receipt_root: receipt_root.as_deref(),
    };
    let mut git = crate::candidate_review_mutation::git::GitInputs::new(&source);
    let verified = evidence::verify_captured_native_with_supporting_profiles(
        &json!({"candidate_paths":paths,"campaigns":catalog.contracts,"runtime":legacy.runtime,"broken_campaigns":catalog.broken,"malformed_receipts":catalog.malformed}),
        &catalog.receipts,
        |path, bytes, contract| crate::candidate_review_mutation::anchor::prove_fixed(&mut git, path, bytes, contract).map_err(|error| format!("{error:#}")),
        fingerprint.as_deref(), installed, supporting.as_ref().map(Option::as_ref).map_err(String::as_str), Some(&bindings)
    ).map_err(anyhow::Error::msg)?;
    let (mut result, code) = projection(verified.result, &registry_path, &request.selection)?;
    if !legacy.diagnostics.is_empty() {
        result["identity_diagnostics"] = json!(legacy.diagnostics);
    }
    git.verify()?;
    capture.verify()?;
    legacy.verify()?;
    if let Some(pin) = &pin {
        pin.verify()?;
    }
    anyhow::ensure!(
        paths
            == crate::mutation_evidence_git_inventory::select(
                &source,
                &mut capture,
                &request.selection,
                request.include_untracked,
                SKIP
            )?,
        "test inventory changed while reporting"
    );
    capture.verify()?;
    Ok((result, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_modes_preserve_debt_exit_contract() {
        let result = json!({"status":"FAIL","evidence":[],"missing_evidence":[{"path":"tests/test_x.py","reason_kind":"no_campaign","receipt_rejections":[]}],"rejection_counts":{"no_campaign":1},"malformed_receipts":[]});
        assert_eq!(
            projection(
                result.clone(),
                "campaigns/registry.json",
                &Selection::Explicit(vec![])
            )
            .unwrap()
            .1,
            5
        );
        assert_eq!(
            projection(
                result.clone(),
                "campaigns/registry.json",
                &Selection::Changed(None)
            )
            .unwrap()
            .1,
            6
        );
        assert_eq!(
            projection(
                result.clone(),
                "campaigns/registry.json",
                &Selection::Coverage
            )
            .unwrap()
            .1,
            5
        );
        assert_eq!(
            projection(result, "campaigns/registry.json", &Selection::Canary)
                .unwrap()
                .1,
            0
        );
    }
    #[test]
    fn malformed_receipt_is_validator_error_for_changed_and_canary() {
        let result = json!({"status":"PASS","evidence":[],"missing_evidence":[],"rejection_counts":{"decode_error":1},"malformed_receipts":["receipts/bad.json"]});
        assert_eq!(
            projection(result.clone(), "r.json", &Selection::Changed(None))
                .unwrap()
                .1,
            5
        );
        assert_eq!(
            projection(result, "r.json", &Selection::Canary).unwrap().1,
            5
        );
    }
    #[test]
    fn configured_registry_and_untracked_inventory_are_observed() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join("custom")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.conductor]\nmutation_registry = 'custom/registry.json'\n",
        )
        .unwrap();
        let manifest =
            "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
        for (path, bytes) in [
            (
                manifest,
                include_bytes!(
                    "../../conductor-native/tests/fixtures/mutation_verifier/manifest.json"
                )
                .as_slice(),
            ),
            (
                "src/conductor/mutation_attribution.py",
                include_bytes!(
                    "../../conductor-native/tests/fixtures/mutation_verifier/source.py.txt"
                )
                .as_slice(),
            ),
            (
                "src/conductor/test_mutation_attribution.py",
                include_bytes!(
                    "../../conductor-native/tests/fixtures/mutation_verifier/test.py.txt"
                )
                .as_slice(),
            ),
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        std::fs::write(dir.path().join("custom/registry.json"), json!({"schema_version":1,"enforcement":"changed_tests","test_patterns":evidence::canonical_test_patterns(),"receipt_directories":[],"campaigns":[{"manifest":manifest}]}).to_string()).unwrap();
        std::fs::write(
            dir.path().join("tests/test_untracked.py"),
            "def test_one(): pass\n",
        )
        .unwrap();
        let request = ReportRequest {
            repo_root: dir.path().into(),
            registry: None,
            selection: Selection::Coverage,
            legacy_component_root: None,
            include_untracked: true,
            native_runner_pin: None,
        };
        let (result, code) = run_report(&request, None).unwrap();
        assert_eq!(code, 5);
        assert_eq!(result["registry"], "custom/registry.json");
        assert_eq!(result["missing_test_files"], 2);
        let without = ReportRequest {
            include_untracked: false,
            ..request
        };
        let (result, code) = run_report(&without, None).unwrap();
        assert_eq!(code, 0);
        assert_eq!(result["total_test_files"], 0);
    }
}
