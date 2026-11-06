//! Opt-in real automatic engine control; operator inputs grant every tool/read root.
use super::*;

#[path = "mutation_cli_fest.rs"]
mod fest;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

const SELECTED: &str = "crates/workspace/member/src/selected.rs";
const SOURCE: &str = "pub fn alpha() -> bool { true }\npub fn beta() -> bool { true }\n";
const MARKER: &str = "forge-timeout-mutant-observed";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    schema_version: u32,
    programs: BTreeMap<String, Program>,
    setup_git: Program,
    read_only: Vec<PathBuf>,
    cgroup_parent: PathBuf,
}

fn bounded(path: &Path, limit: u64) -> Vec<u8> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    let before = file.metadata().unwrap();
    assert!(before.is_file() && before.nlink() == 1 && before.len() <= limit);
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() as u64 <= limit);
    let stamp = |metadata: fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    assert_eq!(stamp(before), stamp(file.metadata().unwrap()));
    assert_eq!(
        stamp(file.metadata().unwrap()),
        stamp(fs::symlink_metadata(path).unwrap())
    );
    bytes
}

fn verify_program(program: &Program) {
    assert!(program.path.is_absolute());
    assert_eq!(program.path.canonicalize().unwrap(), program.path);
    let bytes = bounded(&program.path, 512 * 1024 * 1024);
    assert_eq!(format!("{:x}", Sha256::digest(bytes)), program.sha256);
}

fn configuration() -> Configuration {
    let path = PathBuf::from(
        std::env::var_os("FORGE_TEST_MUTATION_CONFIG")
            .expect("explicit hash-pinned real mutation configuration required"),
    );
    assert!(path.is_absolute());
    assert_eq!(path.canonicalize().unwrap(), path);
    let bytes = bounded(&path, 1024 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        std::env::var("FORGE_TEST_MUTATION_CONFIG_SHA256").expect("configuration digest required")
    );
    let value =
        conductor_native::mutation_evidence::parse_request(std::str::from_utf8(&bytes).unwrap())
            .unwrap();
    let config: Configuration = serde_json::from_value(value).unwrap();
    assert_eq!(config.schema_version, 1);
    assert!(config.programs.len() <= 32);
    for name in ["cargo", "rustc", "rustdoc", "cargo-mutants"] {
        assert!(
            config.programs.contains_key(name),
            "missing operator tool {name}"
        );
    }
    for program in config
        .programs
        .values()
        .chain(std::iter::once(&config.setup_git))
    {
        verify_program(program);
    }
    for path in config
        .read_only
        .iter()
        .chain(std::iter::once(&config.cgroup_parent))
    {
        assert!(path.is_absolute());
        assert_eq!(path.canonicalize().unwrap(), *path);
    }
    config
}

fn put(root: &Path, path: &str, text: &str) {
    let mut directory = root.to_owned();
    for component in Path::new(path).parent().unwrap().components() {
        directory.push(component);
        private_directory(&directory);
    }
    let path = root.join(path);
    fs::write(&path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn retain_configuration(root: &Path) {
    let path = PathBuf::from(std::env::var_os("FORGE_TEST_MUTATION_CONFIG").unwrap());
    let expected = std::env::var("FORGE_TEST_MUTATION_CONFIG_SHA256").unwrap();
    let bytes = bounded(&path, 1024 * 1024);
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
    let retained = root.join("operator-config.json");
    fs::write(&retained, bytes).unwrap();
    fs::set_permissions(&retained, fs::Permissions::from_mode(0o600)).unwrap();
    let record = root.join("operator-inputs.json");
    fs::write(
        &record,
        json!({"config_path":path,"config_sha256":expected,
        "retained_config":"operator-config.json","forge_binary":forge_binary::path(),
        "scope":"operator input provenance; not execution attestation"})
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(record, fs::Permissions::from_mode(0o600)).unwrap();
}

fn fixture(config: &Configuration) -> CargoFixture {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    retain_configuration(root.path());
    let repo = root.path().join("repo");
    for name in [
        "repo",
        "scratch",
        "home",
        "cargo-home",
        "logs",
        "empty-hooks",
    ] {
        private_directory(&root.path().join(name));
    }
    put(&repo, "forge.toml", "schema_version=1\n[conductor]\nmutation_registry='campaigns/registry.json'\nmutation_receipt_root='campaigns/receipts'\n");
    let registry = json!({"schema_version":1,"enforcement":"changed_tests","campaigns":[],
        "test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),
        "receipt_directories":["campaigns/receipts"]});
    put(&repo, "campaigns/registry.json", &registry.to_string());
    put(&repo, ".gitignore", "/campaigns/\n**/target/\n");
    put(
        &repo,
        "crates/workspace/Cargo.toml",
        "[workspace]\nmembers=['member']\nresolver='2'\n",
    );
    put(
        &repo,
        "crates/workspace/member/Cargo.toml",
        "[package]\nname='timeout_subject'\nversion='0.1.0'\nedition='2021'\n",
    );
    put(&repo, SELECTED, SOURCE);
    put(&repo, "crates/workspace/member/src/lib.rs", &format!(
        "mod selected;\n#[cfg(test)] mod tests {{ #[test] fn contract() {{\nif !super::selected::alpha() || !super::selected::beta() {{\nlet marker=std::path::PathBuf::from(std::env::var_os(\"CARGO_TARGET_DIR\").unwrap()).join(\"{MARKER}\");\nstd::fs::write(marker,b\"workspace-mutant-active\\n\").unwrap();\nstd::thread::sleep(std::time::Duration::from_secs(30));\n}}\nassert!(super::selected::alpha()); assert!(super::selected::beta());\n}} }}\n"));
    let programs: serde_json::Map<String, Value> = config
        .programs
        .iter()
        .map(|(name, pin)| (name.clone(), json!({"path":pin.path,"sha256":pin.sha256})))
        .collect();
    let grant = root.path().join("grant.json");
    let value = json!({"version":1,"source_root":repo,"scratch_parent":root.path().join("scratch"),
        "cgroup_parent":config.cgroup_parent,"read_only":config.read_only,"programs":programs,
        "resources":{"cpu_millis":1000,"memory_mib":1024,"processes":64},"max_wall_seconds":30,
        "cargo_workspace":{"lockfile":"crates/workspace/Cargo.lock"}});
    fs::write(&grant, value.to_string()).unwrap();
    fs::set_permissions(&grant, fs::Permissions::from_mode(0o600)).unwrap();
    let pin = digest(&grant);
    CargoFixture {
        root: Some(root),
        repo,
        grant,
        pin,
        uncertain: std::rc::Rc::new(std::cell::Cell::new(true)),
    }
}

fn command(host: &CargoFixture, config: &Configuration, program: &Path) -> Command {
    let mut command = Command::new(program);
    let root = host.root.as_ref().unwrap().path();
    command
        .current_dir(&host.repo)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CARGO_HOME", root.join("cargo-home"))
        .env("TMPDIR", root.join("scratch"))
        .env("PATH", config.setup_git.path.parent().unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("RUSTC", &config.programs["rustc"].path)
        .env("RUSTDOC", &config.programs["rustdoc"].path);
    command
}

fn output(
    host: &CargoFixture,
    command: &mut Command,
    name: &str,
    seconds: u64,
) -> std::process::Output {
    host.uncertain.set(true);
    let owner = OwnedChild {
        child: Some(
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ),
        uncertain: host.uncertain.clone(),
        receipt: host.repo.join(RECEIPT),
    };
    let output = owner.capture(Duration::from_secs(seconds));
    let logs = host.root.as_ref().unwrap().path().join("logs");
    fs::write(logs.join(format!("{name}.stdout")), &output.stdout).unwrap();
    fs::write(logs.join(format!("{name}.stderr")), &output.stderr).unwrap();
    fs::write(logs.join(format!("{name}.command.json")), json!({"program":command.get_program().to_string_lossy(),
        "argv":command.get_args().map(|arg|arg.to_string_lossy()).collect::<Vec<_>>(),"returncode":output.status.code()}).to_string()).unwrap();
    output
}

fn git_command(host: &CargoFixture, config: &Configuration, args: &[&str], name: &str) {
    let mut cmd = command(host, config, &config.setup_git.path);
    cmd.args([
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ]);
    cmd.arg("-c").arg(format!(
        "core.hooksPath={}",
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("empty-hooks")
            .display()
    ));
    let output = output(host, cmd.args(args), name, 10);
    assert!(
        output.status.success(),
        "Git preparation {name}: {output:?}"
    );
}

fn prepare(host: &CargoFixture, config: &Configuration) {
    let mut engine = command(host, config, &config.programs["cargo-mutants"].path);
    let version = output(
        host,
        engine.args(["mutants", "--version"]),
        "engine-version",
        10,
    );
    assert!(version.status.success());
    assert_eq!(
        std::str::from_utf8(&version.stdout).unwrap().trim(),
        "cargo-mutants 27.1.0"
    );
    let mut cargo = command(host, config, &config.programs["cargo"].path);
    let result = output(
        host,
        cargo.args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            "crates/workspace/Cargo.toml",
        ]),
        "fixture-lock",
        15,
    );
    assert!(result.status.success(), "{result:?}");
    let template = format!(
        "--template={}",
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("empty-hooks")
            .display()
    );
    git_command(
        host,
        config,
        &["init", "-q", "-b", "main", &template],
        "init",
    );
    fs::set_permissions(host.repo.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    git_command(
        host,
        config,
        &["add", "forge.toml", ".gitignore", "crates"],
        "base-add",
    );
    git_command(
        host,
        config,
        &["add", "-f", "campaigns/registry.json"],
        "registry-add",
    );
    git_command(
        host,
        config,
        &["commit", "-qm", "authored baseline"],
        "base-commit",
    );
    git_command(
        host,
        config,
        &["checkout", "-qb", "timeout-control"],
        "branch",
    );
    put(
        &host.repo,
        SELECTED,
        &format!("{SOURCE}// selected changed source\n"),
    );
    git_command(host, config, &["add", SELECTED], "source-add");
    git_command(
        host,
        config,
        &["commit", "-qm", "select two boolean functions"],
        "source-commit",
    );
}

fn forge(
    host: &CargoFixture,
    config: &Configuration,
    args: &[&str],
    name: &str,
    seconds: u64,
) -> (std::process::ExitStatus, Value) {
    let mut cmd = command(host, config, Path::new(forge_binary::path()));
    let result = output(host, cmd.args(args), name, seconds);
    let value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|error| panic!("{name}: {error}: {result:?}"));
    (result.status, value)
}

fn expected_manifest(host: &CargoFixture, value: &Value) {
    assert_eq!(value["language"], "rust");
    assert_eq!(value["mutation_engine"], "cargo-mutants");
    assert_eq!(value["generator"]["source"], json!(["src/selected.rs"]));
    assert_eq!(value["generator"]["jobs"], 1);
    assert_eq!(value["generator"]["run_timeout_seconds"], 10);
    let options = &value["generator"]["options"];
    assert_eq!(options["execution_profile"], "cargo-workspace-v2");
    assert_eq!(options["package"], "timeout_subject");
    assert_eq!(
        options["manifest_path"],
        "crates/workspace/member/Cargo.toml"
    );
    assert_eq!(options["package_root"], "crates/workspace/member");
    assert_eq!(
        value["test_argv"],
        json!([
            "cargo",
            "test",
            "--manifest-path",
            "crates/workspace/member/Cargo.toml",
            "--package",
            "timeout_subject"
        ])
    );
    assert_eq!(
        value["source_sha256"],
        json!({SELECTED:digest(&host.repo.join(SELECTED))})
    );
    let test = "crates/workspace/member/src/lib.rs";
    assert_eq!(
        value["test_sha256"],
        json!({test:digest(&host.repo.join(test))})
    );
}

fn generate(host: &CargoFixture, config: &Configuration) -> (PathBuf, Value, String) {
    let args = [
        "rust",
        "--repo-root",
        host.repo.to_str().unwrap(),
        "--owner",
        "lane",
        "--base",
        "main",
        "--only",
        SELECTED,
        "--cargo-workspace",
        "--jobs",
        "1",
        "--run-timeout",
        "10",
    ];
    for mode in ["plan", "write"] {
        let argv: Vec<_> = ["mutation", mode].into_iter().chain(args).collect();
        let (status, value) = forge(host, config, &argv, mode, 15);
        assert!(status.success(), "{value}");
        if mode == "write" {
            let paths: Vec<_> = value["written"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .filter(|path| !path.contains("/registry.d/"))
                .collect();
            assert_eq!(paths.len(), 1);
            let manifest = host.repo.join(paths[0]);
            assert_eq!(manifest.parent().unwrap(), host.repo.join("campaigns"));
            let bytes = bounded(&manifest, 2 * 1024 * 1024);
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            expected_manifest(host, &value);
            fs::write(
                host.root
                    .as_ref()
                    .unwrap()
                    .path()
                    .join("logs/manifest-before.json"),
                &bytes,
            )
            .unwrap();
            return (manifest, value, format!("{:x}", Sha256::digest(bytes)));
        }
    }
    unreachable!()
}

fn verify_manifest_binding(
    host: &CargoFixture,
    receipt: &Value,
    path: &Path,
    before: &Value,
    before_sha: &str,
) {
    let bytes = bounded(path, 2 * 1024 * 1024);
    let after: Value = serde_json::from_slice(&bytes).unwrap();
    expected_manifest(host, &after);
    for field in [
        "campaign_id",
        "generator",
        "test_argv",
        "source_sha256",
        "test_sha256",
    ] {
        assert_eq!(
            after[field], before[field],
            "native final manifest changed scope {field}"
        );
    }
    let final_sha = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(receipt["manifest_sha256"], final_sha);
    assert_eq!(
        receipt["manifest"],
        path.strip_prefix(&host.repo).unwrap().to_str().unwrap()
    );
    for field in ["campaign_id", "source_sha256", "test_sha256"] {
        assert_eq!(
            receipt[field], after[field],
            "receipt differs from automatic manifest {field}"
        );
    }
    fs::write(
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("logs/manifest-binding.json"),
        json!({"before_sha256":before_sha,"after_sha256":final_sha,
            "scope":"native final manifest after automatic baseline recording; no fixture edits"})
        .to_string(),
    )
    .unwrap();
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(rows) => {
            let sorted: BTreeMap<_, _> = rows
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(rows) => Value::Array(rows.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn identity(value: &Value) -> String {
    let mut value = value.clone();
    value.as_object_mut().unwrap().remove("diff");
    canonical(&value).to_string()
}

fn identity_digest(rows: &BTreeSet<String>) -> String {
    let mut hash = Sha256::new();
    for row in rows {
        hash.update((row.len() as u64).to_be_bytes());
        hash.update(row.as_bytes());
    }
    format!("{hash:x}", hash = hash.finalize())
}

fn reports(snapshot: &Path, progress: &Value) {
    let root = snapshot.join("mutants-output/mutants.out");
    let inventory_bytes = bounded(&root.join("mutants.json"), 16 * 1024 * 1024);
    let outcome_bytes = bounded(&root.join("outcomes.json"), 16 * 1024 * 1024);
    for (name, bytes) in [
        ("inventory", &inventory_bytes),
        ("outcomes", &outcome_bytes),
    ] {
        assert_eq!(
            progress[name]["sha256"],
            format!("{:x}", Sha256::digest(bytes))
        );
        assert_eq!(progress[name]["bytes"], bytes.len());
    }
    let inventory: Vec<Value> = serde_json::from_slice(&inventory_bytes).unwrap();
    assert_eq!(inventory.len(), 2);
    assert!(inventory
        .iter()
        .all(|row| row["file"] == "member/src/selected.rs" && row["package"] == "timeout_subject"));
    let identities: BTreeSet<_> = inventory.iter().map(identity).collect();
    assert_eq!(identities.len(), inventory.len());
    assert_eq!(
        progress["inventory"]["identities_sha256"],
        identity_digest(&identities)
    );
    let outcome: Value = serde_json::from_slice(&outcome_bytes).unwrap();
    let mut observed = BTreeSet::new();
    let mut counts = json!({"KILLED":0,"SURVIVED":0,"TIMED_OUT":0,"UNVIABLE":0,"ERROR":0});
    for row in outcome["outcomes"].as_array().unwrap() {
        if row["scenario"] == "Baseline" {
            continue;
        }
        let key = identity(&row["scenario"]["Mutant"]);
        assert!(identities.contains(&key) && observed.insert(key));
        let name = match row["summary"].as_str().unwrap() {
            "CaughtMutant" => "KILLED",
            "MissedMutant" => "SURVIVED",
            "Timeout" => "TIMED_OUT",
            "Unviable" => "UNVIABLE",
            "Failure" => "ERROR",
            other => panic!("unknown outcome {other}"),
        };
        counts[name] = json!(counts[name].as_u64().unwrap() + 1);
    }
    assert_eq!(outcome["total_mutants"], observed.len());
    assert_eq!(progress["outcomes"]["observed"], observed.len());
    assert_eq!(progress["outcomes"]["counts"], counts);
    assert_eq!(
        progress["outcomes"]["identities_sha256"],
        identity_digest(&observed)
    );
}

fn verify(host: &CargoFixture, receipt: &Value, original: &str, config: &Configuration) {
    assert_eq!(receipt["status"], "ERROR");
    assert_eq!(receipt["complete_campaign"], false);
    assert_eq!(receipt["interpreter"], Value::Null);
    assert_eq!(receipt["mutation_score"], Value::Null);
    assert_eq!(receipt["mutants"], json!([]));
    assert_eq!(
        receipt["accounting"],
        json!({"planned":null,"completed":0,"failed":0,"censored":0,"unstarted":null})
    );
    assert!(receipt.get("outcome_counts").is_none());
    assert_eq!(receipt["baseline"]["returncode"], 0);
    assert_eq!(receipt["baseline"]["timed_out"], false);
    assert!(
        receipt["workspace_baseline"]["direct"]["passed"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let partial = &receipt["engine_partial_progress"];
    assert!(receipt.get("engine_partial_progress_error").is_none());
    assert_eq!(partial["schema_version"], 1);
    assert_eq!(partial["reason"], "timeout");
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["inventory"]["discovered"], 2);
    assert_eq!(partial["remaining_or_interrupted"], Value::Null);
    let commands = receipt["process_commands"].as_array().unwrap();
    assert_eq!(commands.len(), 3);
    for row in &commands[..2] {
        assert_eq!(row["confinement"]["outcome"], "exited");
        assert_eq!(row["confinement"]["exit_code"], 0);
    }
    assert_eq!(receipt["process_cleanup"]["complete"], true);
    assert_eq!(
        receipt["process_cleanup"]["commands_observed"],
        commands.len()
    );
    for row in commands {
        assert_eq!(row["state"], "observed");
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert_eq!(row["confinement"]["resources"]["verified"], true);
        let group = Path::new(row["confinement"]["resources"]["path"].as_str().unwrap());
        assert!(group.starts_with(&config.cgroup_parent) && group != config.cgroup_parent);
        assert!(absent(group));
    }
    assert_eq!(commands[2]["confinement"]["outcome"], "timed_out");
    let argv = commands[2]["argv"].as_array().unwrap();
    let positions: Vec<_> = argv
        .iter()
        .enumerate()
        .filter(|(_, value)| **value == "--file")
        .collect();
    assert_eq!(positions.len(), 1);
    assert_eq!(argv[positions[0].0 + 1], "member/src/selected.rs");
    assert_eq!(receipt["snapshot"]["removed"], false);
    assert!(receipt["snapshot"]["retained_reason"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(receipt["execution_verification_error"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    let snapshot = Path::new(receipt["snapshot"]["root"].as_str().unwrap());
    assert_eq!(
        snapshot.parent().unwrap(),
        host.root.as_ref().unwrap().path().join("scratch")
    );
    assert_eq!(snapshot.canonicalize().unwrap(), snapshot);
    assert_ne!(
        bounded(&snapshot.join("worktree").join(SELECTED), 1024 * 1024),
        original.as_bytes()
    );
    assert_eq!(
        bounded(&snapshot.join("target").join(MARKER), 1024),
        b"workspace-mutant-active\n"
    );
    assert_eq!(
        fs::read_to_string(host.repo.join(SELECTED)).unwrap(),
        original
    );
    reports(snapshot, partial);
}

#[test]
#[ignore = "requires FORGE_TEST_BIN and explicit pinned FORGE_TEST_MUTATION_CONFIG/SHA256 with delegated cgroup and real cargo-mutants 27.1.0"]
fn real_automatic_workspace_timeout_retains_partial_diagnostics_and_owned_cleanup() {
    let config = configuration();
    let host = fixture(&config);
    let binary_sha = format!(
        "{:x}",
        Sha256::digest(bounded(Path::new(forge_binary::path()), 512 * 1024 * 1024))
    );
    prepare(&host, &config);
    let original = fs::read_to_string(host.repo.join(SELECTED)).unwrap();
    let (manifest, generated_before, manifest_before_sha) = generate(&host, &config);
    assert_eq!(
        format!("{:x}", Sha256::digest(bounded(&manifest, 2 * 1024 * 1024))),
        manifest_before_sha
    );
    let (status, shown) = forge(
        &host,
        &config,
        &[
            "mutation",
            "run",
            manifest.to_str().unwrap(),
            "--repo-root",
            host.repo.to_str().unwrap(),
            "--allow-mutations",
            "--owner",
            "lane",
            "--base",
            "main",
            "--receipt",
            RECEIPT,
            "--execution-grants",
            host.grant.to_str().unwrap(),
            "--execution-grants-sha256",
            &host.pin,
        ],
        "run",
        75,
    );
    assert_eq!(status.code(), Some(REFUSED), "{shown}");
    let (shown_status, receipt) = forge(
        &host,
        &config,
        &["receipt", "show", host.repo.join(RECEIPT).to_str().unwrap()],
        "receipt",
        10,
    );
    assert!(shown_status.success());
    assert_eq!(receipt["native_runner"]["source_matches_git"], true);
    assert_eq!(receipt["native_runner"]["executable_sha256"], binary_sha);
    verify_manifest_binding(
        &host,
        &receipt,
        &manifest,
        &generated_before,
        &manifest_before_sha,
    );
    verify(&host, &receipt, &original, &config);
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(bounded(Path::new(forge_binary::path()), 512 * 1024 * 1024))
        ),
        binary_sha
    );
    for program in config
        .programs
        .values()
        .chain(std::iter::once(&config.setup_git))
    {
        verify_program(program);
    }
    eprintln!(
        "retained real automatic timeout evidence: {}",
        host.root.as_ref().unwrap().path().display()
    );
}
