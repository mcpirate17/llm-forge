//! Opt-in automatic Mull controls, with explicit operator-supplied toolchain pins.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const SOURCE: &str = "src/subject.cpp";
const DRIVER: &str = "tests/driver.cpp";
const SUBJECT: &str = "int answer(int value) { return value + 1; }\n";
const STRONG: &str = "extern int answer(int); int main() { return answer(0) != 1 || answer(1) != 2 || answer(-1) != 0; }\n";
const WEAK: &str = "extern int answer(int); int main() { volatile int result = answer(1); (void)result; return 0; }\n";
const RED: &str = "extern int answer(int); int main() { (void)answer(1); return 1; }\n";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    programs: BTreeMap<String, Program>,
    setup_git: Program,
    read_only: Vec<PathBuf>,
    cgroup_parent: PathBuf,
    mull_toolchain: Value,
}
fn config() -> (Config, Vec<u8>) {
    let path = PathBuf::from(
        std::env::var_os("FORGE_TEST_MULL_CONFIG").expect("operator Mull config required"),
    );
    assert!(path.is_absolute() && path.canonicalize().unwrap() == path);
    let bytes = bounded(&path, 1024 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        std::env::var("FORGE_TEST_MULL_CONFIG_SHA256").expect("operator config hash required")
    );
    let value =
        conductor_native::mutation_evidence::parse_request(std::str::from_utf8(&bytes).unwrap())
            .unwrap();
    let cfg: Config = serde_json::from_value(value).unwrap();
    assert_eq!(cfg.schema_version, 1);
    let expected: BTreeSet<_> = [
        "cmake",
        "ninja",
        "clang",
        "ld",
        "llvm-profdata",
        "mull-runner",
        "sh",
        "uname",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        cfg.programs
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected
    );
    for program in cfg.programs.values().chain(std::iter::once(&cfg.setup_git)) {
        verify_program(program);
    }
    for path in cfg
        .read_only
        .iter()
        .chain(std::iter::once(&cfg.cgroup_parent))
    {
        assert!(path.is_absolute() && path.canonicalize().unwrap() == *path);
    }
    assert_eq!(cfg.mull_toolchain["llvm_major"], 18);
    (cfg, bytes)
}
fn command(host: &CargoFixture, cfg: &Config, binary: &Path) -> Command {
    let root = host.root.as_ref().unwrap().path();
    let mut command = Command::new(binary);
    command
        .current_dir(&host.repo)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", cfg.setup_git.path.parent().unwrap())
        .env("TMPDIR", root.join("scratch"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}
fn git(host: &CargoFixture, cfg: &Config, args: &[&str], label: &str) {
    let mut cmd = command(host, cfg, &cfg.setup_git.path);
    cmd.args([
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ])
    .arg("-c")
    .arg(format!(
        "core.hooksPath={}",
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("empty-hooks")
            .display()
    ))
    .args(args);
    let result = output(host, &mut cmd, label, 15);
    assert!(result.status.success(), "{label}: {result:?}");
}
fn grant(cfg: &Config, repo: &Path, root: &Path, wrong_plugin: bool) -> Value {
    let programs: serde_json::Map<String, Value> = cfg
        .programs
        .iter()
        .map(|(name, program)| {
            (
                name.clone(),
                json!({"path":program.path,"sha256":program.sha256}),
            )
        })
        .collect();
    let mut toolchain = cfg.mull_toolchain.clone();
    if wrong_plugin {
        let plugin = toolchain["plugin"].as_str().unwrap().to_owned();
        toolchain["files"][&plugin] = json!("0".repeat(64));
    }
    json!({"version":1,"source_root":repo,"scratch_parent":root.join("scratch"),"cgroup_parent":cfg.cgroup_parent,
        "read_only":cfg.read_only,"programs":programs,"resources":{"cpu_millis":1000,"memory_mib":1024,"processes":64},"max_wall_seconds":60,"mull_toolchain":toolchain})
}
fn fixture(cfg: &Config, config_bytes: &[u8], driver: &str, wrong_plugin: bool) -> CargoFixture {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["repo", "scratch", "home", "logs", "empty-hooks"] {
        private_directory(&root.path().join(name));
    }
    let repo = root.path().join("repo");
    put(&repo, "forge.toml", "schema_version=1\n[conductor]\nmutation_registry='campaigns/registry.json'\nmutation_receipt_root='campaigns/receipts'\n");
    put(&repo, ".gitignore", "/campaigns/\n");
    put(&repo, SOURCE, SUBJECT);
    put(&repo, DRIVER, driver);
    let registry = json!({"schema_version":1,"enforcement":"changed_tests","campaigns":[],"test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),"receipt_directories":["campaigns/receipts"]});
    put(&repo, "campaigns/registry.json", &registry.to_string());
    fs::write(root.path().join("operator-config.json"), config_bytes).unwrap();
    let grant_path = root.path().join("grant.json");
    fs::write(
        &grant_path,
        serde_json::to_vec(&grant(cfg, &repo, root.path(), wrong_plugin)).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&grant_path, fs::Permissions::from_mode(0o600)).unwrap();
    let pin = digest(&grant_path);
    let host = CargoFixture {
        root: Some(root),
        repo,
        grant: grant_path,
        pin,
        uncertain: std::rc::Rc::new(std::cell::Cell::new(true)),
    };
    initialize(&host, cfg);
    host
}
fn initialize(host: &CargoFixture, cfg: &Config) {
    let template = format!(
        "--template={}",
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("empty-hooks")
            .display()
    );
    git(host, cfg, &["init", "-q", "-b", "main", &template], "init");
    fs::set_permissions(host.repo.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    git(
        host,
        cfg,
        &["add", "forge.toml", ".gitignore", SOURCE, DRIVER],
        "add",
    );
    git(
        host,
        cfg,
        &["add", "-f", "campaigns/registry.json"],
        "registry",
    );
    git(host, cfg, &["commit", "-qm", "base"], "base");
    git(host, cfg, &["checkout", "-qb", "mull-control"], "branch");
    put(&host.repo, SOURCE, &format!("{SUBJECT}// scoped change\n"));
    git(host, cfg, &["add", SOURCE], "changed-add");
    git(host, cfg, &["commit", "-qm", "selected source"], "changed");
}
fn forge(
    host: &CargoFixture,
    cfg: &Config,
    args: &[&str],
    label: &str,
    seconds: u64,
) -> (std::process::ExitStatus, Value) {
    let mut cmd = command(host, cfg, Path::new(forge_binary::path()));
    let output = output(host, cmd.args(args), label, seconds);
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{label}: {e}: {output:?}"));
    (output.status, value)
}
fn generate(host: &CargoFixture, cfg: &Config) -> (PathBuf, Value, String) {
    let args = [
        "cpp",
        "--repo-root",
        host.repo.to_str().unwrap(),
        "--owner",
        "lane",
        "--base",
        "main",
        "--only",
        SOURCE,
        "--mull-confined",
        "--jobs",
        "1",
        "--run-timeout",
        "60",
        "--extra-test",
        "src/subject.cpp=tests/driver.cpp",
    ];
    let (status, planned) = forge(
        host,
        cfg,
        &[&["mutation", "plan"][..], &args].concat(),
        "plan",
        30,
    );
    assert!(status.success(), "plan: {planned}");
    let (status, written) = forge(
        host,
        cfg,
        &[&["mutation", "write"][..], &args].concat(),
        "write",
        30,
    );
    assert!(status.success(), "write: {written}");
    let paths: Vec<_> = written["written"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|path| !path.contains("/registry.d/"))
        .collect();
    assert_eq!(paths.len(), 1);
    let path = host.repo.join(paths[0]);
    assert_eq!(path.parent().unwrap(), host.repo.join("campaigns"));
    let bytes = bounded(&path, 2 * 1024 * 1024);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["language"], "cpp");
    assert_eq!(value["mutation_engine"], "mull");
    assert_eq!(value["generator"]["source"], json!([SOURCE]));
    assert_eq!(
        value["generator"]["options"],
        json!({"execution_profile":"mull-confined-v1"})
    );
    assert_eq!(value["generator"]["jobs"], 1);
    assert_eq!(value["test_argv"], json!(["forge-mull-test"]));
    assert_eq!(
        value["source_sha256"],
        json!({SOURCE:digest(&host.repo.join(SOURCE))})
    );
    assert_eq!(
        value["test_sha256"],
        json!({DRIVER:digest(&host.repo.join(DRIVER))})
    );
    fs::write(
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("logs/manifest-before.json"),
        &bytes,
    )
    .unwrap();
    (path, value, format!("{:x}", Sha256::digest(bytes)))
}

fn verify_tools(host: &CargoFixture, cfg: &Config, stage: &str) {
    for program in cfg.programs.values().chain(std::iter::once(&cfg.setup_git)) {
        verify_program(program);
    }
    for (path, hash) in cfg.mull_toolchain["files"].as_object().unwrap() {
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(bounded(Path::new(path), 256 * 1024 * 1024))
            ),
            hash.as_str().unwrap()
        );
    }
    for (index, (path, hash)) in cfg.mull_toolchain["directories"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
    {
        let label = format!("inventory-{stage}-{index}");
        let (status, inventory) = forge(
            host,
            cfg,
            &["mutation", "mull-inventory", "--directory", path],
            &label,
            120,
        );
        assert!(status.success(), "{label}: {inventory}");
        assert_eq!(inventory["directory"], *path);
        assert_eq!(inventory["sha256"], *hash);
    }
    let path = PathBuf::from(std::env::var_os("FORGE_TEST_MULL_CONFIG").unwrap());
    assert_eq!(
        format!("{:x}", Sha256::digest(bounded(&path, 1024 * 1024))),
        std::env::var("FORGE_TEST_MULL_CONFIG_SHA256").unwrap()
    );
}
fn tracked(host: &CargoFixture, cfg: &Config, label: &str) -> BTreeMap<String, String> {
    let mut cmd = command(host, cfg, &cfg.setup_git.path);
    let result = output(host, cmd.args(["ls-files", "-z"]), label, 15);
    assert!(result.status.success());
    result
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path).unwrap().to_owned();
            let hash = digest(&host.repo.join(&path));
            (path, hash)
        })
        .collect()
}
fn cleanup(host: &CargoFixture, cfg: &Config, receipt: &Value) {
    let commands = receipt["process_commands"]
        .as_array()
        .expect("owned command ledger");
    assert!(!commands.is_empty());
    assert_eq!(receipt["process_cleanup"]["complete"], true);
    assert_eq!(
        receipt["process_cleanup"]["commands_observed"],
        commands.len()
    );
    for row in commands {
        assert_eq!(row["state"], "observed");
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert_eq!(row["confinement"]["resources"]["verified"], true);
        let path = Path::new(row["confinement"]["resources"]["path"].as_str().unwrap());
        assert!(path.starts_with(&cfg.cgroup_parent) && path != cfg.cgroup_parent && absent(path));
    }
    for field in ["snapshot", "mull_definition"] {
        let path = Path::new(
            receipt[field]["root"]
                .as_str()
                .or_else(|| receipt[field]["path"].as_str())
                .unwrap(),
        );
        assert_eq!(
            path.parent().unwrap(),
            host.root.as_ref().unwrap().path().join("scratch")
        );
        assert_eq!(receipt[field]["removed"], true, "{field}");
        assert!(absent(path), "{field} remains: {}", path.display());
    }
    assert_eq!(receipt["mull_definition"]["read_only"], true);
}
fn manifest_check(host: &CargoFixture, path: &Path, before: &Value, receipt: &Value) {
    let bytes = bounded(path, 2 * 1024 * 1024);
    let after: Value = serde_json::from_slice(&bytes).unwrap();
    for key in [
        "campaign_id",
        "generator",
        "test_argv",
        "source_sha256",
        "test_sha256",
    ] {
        assert_eq!(after[key], before[key], "manifest changed: {key}");
    }
    assert_eq!(receipt["campaign_id"], after["campaign_id"]);
    assert_eq!(
        receipt["manifest"].as_str(),
        path.strip_prefix(&host.repo).unwrap().to_str()
    );
    assert_eq!(
        receipt["manifest_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(receipt["source_sha256"], after["source_sha256"]);
    assert_eq!(receipt["test_sha256"], after["test_sha256"]);
    assert_eq!(
        receipt["source_sha256"][SOURCE],
        digest(&host.repo.join(SOURCE))
    );
    assert_eq!(
        receipt["test_sha256"][DRIVER],
        digest(&host.repo.join(DRIVER))
    );
}
fn verify_outcome(receipt: &Value, manifest: &Value, strong: bool, code: std::process::ExitStatus) {
    let rows = receipt["mutants"]
        .as_array()
        .expect("automatic Elements outcomes");
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row["path"] == SOURCE));
    assert_eq!(receipt["complete_campaign"], true);
    assert_eq!(receipt["accounting"]["planned"], rows.len());
    assert_eq!(receipt["accounting"]["completed"], rows.len());
    for field in ["failed", "censored", "unstarted"] {
        assert_eq!(receipt["accounting"][field], 0, "{field}");
    }
    assert_eq!(receipt["source_coverage"]["complete"], true);
    assert_eq!(receipt["source_coverage"]["missing"], json!([]));
    assert_eq!(
        receipt["source_coverage"]["scored_by_source"][SOURCE],
        rows.len()
    );
    assert!(
        conductor_native::mutation_source_coverage::validate(receipt.as_object().unwrap()).is_ok()
    );
    assert_eq!(
        receipt["mull_observation"]["baselines"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(conductor_native::mutation_mull_receipt::evidence_error(
        receipt.as_object().unwrap(),
        manifest["campaign_id"].as_str().unwrap(),
        &manifest["source_sha256"],
        &manifest["test_sha256"]
    )
    .is_none());
    if strong {
        assert!(code.success(), "strong control: {receipt}");
        assert_eq!(receipt["status"], "PASS");
        assert!(rows.iter().all(|row| row["outcome"] == "KILLED"));
    } else {
        assert!(code.success(), "RATCHET_HELD is an accepted CLI verdict");
        assert_eq!(receipt["status"], "RATCHET_HELD");
        assert!(rows.iter().any(|row| row["outcome"] == "SURVIVED"));
    }
}
fn verify_failed_baseline(receipt: &Value, cfg: &Config) {
    assert_eq!(receipt["status"], "BASELINE_FAILED");
    let builds = receipt["build"]
        .as_array()
        .expect("configure and build outcomes");
    assert_eq!(builds.len(), 2);
    assert!(builds
        .iter()
        .all(|row| row["returncode"] == 0 && row["timed_out"] == false));
    assert_eq!(receipt["baseline_argv"], json!(["forge-mull-test"]));
    assert_eq!(receipt["baseline"]["returncode"], 1);
    assert_eq!(receipt["baseline"]["timed_out"], false);
    let commands = receipt["process_commands"]
        .as_array()
        .expect("owned commands");
    assert!(commands
        .iter()
        .any(|row| row["argv"] == json!(["forge-mull-test"])
            && row["confinement"]["outcome"] == "exited"
            && row["confinement"]["exit_code"] == 1
            && row["confinement"]["signal"].is_null()));
    assert!(!commands
        .iter()
        .any(|row| row["argv"].as_array().is_some_and(|argv| {
            argv.first().and_then(Value::as_str).is_some_and(|program| {
                program == "mull-runner" || Path::new(program) == cfg.programs["mull-runner"].path
            })
        })));
}

fn run_command(
    host: &CargoFixture,
    cfg: &Config,
    manifest: &Path,
) -> (std::process::ExitStatus, Value) {
    forge(
        host,
        cfg,
        &[
            "mutation",
            "run",
            manifest.to_str().unwrap(),
            "--repo-root",
            host.repo.to_str().unwrap(),
            "--owner",
            "lane",
            "--base",
            "main",
            "--allow-mutations",
            "--receipt",
            RECEIPT,
            "--execution-grants",
            host.grant.to_str().unwrap(),
            "--execution-grants-sha256",
            &host.pin,
        ],
        "run",
        180,
    )
}
fn receipt(host: &CargoFixture, cfg: &Config) -> Value {
    let (status, value) = forge(
        host,
        cfg,
        &["receipt", "show", host.repo.join(RECEIPT).to_str().unwrap()],
        "receipt",
        15,
    );
    assert!(status.success(), "receipt: {value}");
    value
}
fn exercise(driver: &str, kind: &str) {
    let (cfg, bytes) = config();
    let wrong = kind == "wrong_plugin";
    let host = fixture(&cfg, &bytes, driver, wrong);
    eprintln!(
        "{}",
        json!({"kind":"mull-fixture","control":kind,"root":host.root.as_ref().unwrap().path(),"state":"retained"})
    );
    let forge_path = Path::new(forge_binary::path());
    let forge_hash = format!(
        "{:x}",
        Sha256::digest(bounded(forge_path, 512 * 1024 * 1024))
    );
    verify_tools(&host, &cfg, "before");
    let before = tracked(&host, &cfg, "tracked-before");
    let (manifest, generated, generated_hash) = generate(&host, &cfg);
    assert_eq!(digest(&manifest), generated_hash);
    let (status, printed) = run_command(&host, &cfg, &manifest);
    let recorded = if wrong {
        assert!(!status.success());
        assert_eq!(printed["status"], "REFUSED");
        let error = printed["error"].as_str().expect("refusal diagnostic");
        assert!(error.contains("digest mismatch"));
        assert!(error.contains(cfg.mull_toolchain["plugin"].as_str().unwrap()));
        assert!(absent(&host.repo.join(RECEIPT)));
        assert!(
            fs::read_dir(host.root.as_ref().unwrap().path().join("scratch"))
                .unwrap()
                .next()
                .is_none()
        );
        Value::Null
    } else {
        let receipt = receipt(&host, &cfg);
        assert_eq!(receipt["native_runner"]["executable_sha256"], forge_hash);
        if kind == "baseline_failure" {
            assert!(!status.success());
            verify_failed_baseline(&receipt, &cfg);
        } else {
            verify_outcome(&receipt, &generated, kind == "strong", status);
        }
        manifest_check(&host, &manifest, &generated, &receipt);
        cleanup(&host, &cfg, &receipt);
        receipt
    };
    assert_eq!(tracked(&host, &cfg, "tracked-after"), before);
    verify_tools(&host, &cfg, "after");
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(bounded(forge_path, 512 * 1024 * 1024))
        ),
        forge_hash
    );
    host.settle(&printed, &recorded);
    assert!(!host.uncertain.get(), "owned cleanup remains unverified");
    std::mem::forget(host);
}
#[test]
#[ignore = "requires pinned FORGE_TEST_MULL_CONFIG/SHA256, FORGE_TEST_BIN and delegated cgroup"]
fn real_mull_strong_and_weak_automatic_campaigns() {
    exercise(STRONG, "strong");
    exercise(WEAK, "weak");
}
#[test]
#[ignore = "requires pinned FORGE_TEST_MULL_CONFIG/SHA256, FORGE_TEST_BIN and delegated cgroup"]
fn real_mull_failed_baseline_cannot_produce_pass() {
    exercise(RED, "baseline_failure");
}
#[test]
#[ignore = "requires pinned FORGE_TEST_MULL_CONFIG/SHA256, FORGE_TEST_BIN and delegated cgroup"]
fn real_mull_wrong_plugin_pin_refuses_before_payload() {
    exercise(STRONG, "wrong_plugin");
}
