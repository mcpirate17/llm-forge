//! Explicitly provisioned real Fest controls; generated output is never edited.
use super::*;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

const SOURCE: &str = "def answer(value=7):\n    assert value > 0\n    return value\n";
const COLLECTION_SOURCE: &str = "def answer(value=7):\n    return value + 1\n";
const POLICY: &str = "schema_version=1\nblock_at='high'\nmax_workers=1\ncache_ttl_days=7\nclaim_max_age_hours=24\nmax_file_bytes=100000\nmax_binary_bytes=200000\ncoverage_threshold=80.0\nhigh_risk_coverage_threshold=90.0\nbaseline_expires=2099-11-30\n[classes]\npython=['**/*.py']\ntest=['test_*.py','**/test_*.py']\nnative=['**/*.rs']\n[risk]\nhigh=[]\n[paths]\nprotected_deletes=[]\nhot=[]\ngenerated=[]\n[checks.mutation-evidence]\nkind='builtin'\nprofiles=['fast']\nalways=true\n";
const SHADOW_FILES: [&str; 8] = [
    "pytest.py",
    "coverage.py",
    "json.py",
    "pytest_cov/__init__.py",
    "src/pytest.py",
    "src/coverage.py",
    "src/json.py",
    "src/pytest_cov/__init__.py",
];
const TEST_HEAD: &str = r#"import errno, hashlib, importlib, json, os, sys, time
import pytest, pytest_cov, coverage

def test_answer():
    import subject
    normal = subject.answer()
    normal_loader = type(subject.__loader__).__name__
    del sys.modules['subject']
    pytest.register_assert_rewrite('subject')
    rewritten = importlib.import_module('subject')
    rewritten_value = rewritten.answer()
    source_path = rewritten.__file__
    data = open(source_path, 'rb').read()
    st = os.stat(source_path)
    denied = False
    try:
        fd = os.open(os.path.join(sys.pycache_prefix, 'probe-' + str(os.getpid())), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(fd)
    except OSError as error:
        denied = error.errno in (errno.EACCES, errno.EPERM, errno.EROFS)
    row = {'pid':os.getpid(),'source':data.decode('utf-8'),'sha256':hashlib.sha256(data).hexdigest(),
           'size':st.st_size,'mtime_sec':int(st.st_mtime),'normal':normal,'rewritten':rewritten_value,
           'normal_loader':normal_loader,'rewritten_loader':type(rewritten.__loader__).__name__,
           'dont_write':sys.dont_write_bytecode,'prefix':sys.pycache_prefix,'write_denied':denied,
           'guest_prefix':sys.prefix, 'origins':{name:os.path.realpath(module.__file__) for name, module in
               [('pytest',pytest),('pytest_cov',pytest_cov),('coverage',coverage),('json',json)]}}
    directory = os.environ['TMPDIR']
    final = os.path.join(directory, 'forge-observation-' + str(os.getpid()) + '.json')
    temporary = final + '.tmp'
    with open(temporary, 'w', encoding='utf-8') as stream:
        json.dump(row, stream, sort_keys=True)
    os.replace(temporary, final)
    time.sleep(0.10)
    assert row['dont_write'] and row['write_denied']
    assert row['rewritten_loader'] == 'AssertionRewritingHook'
"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Program {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    programs: BTreeMap<String, Program>,
    setup_git: Program,
    read_only: Vec<PathBuf>,
    cgroup_parent: PathBuf,
    python_guest: Value,
}
fn hash_file(path: &Path, max: u64) -> String {
    assert!(path.is_absolute() && path.canonicalize().unwrap() == path);
    format!("{:x}", Sha256::digest(bounded(path, max)))
}
fn config() -> (Config, Vec<u8>) {
    let path = PathBuf::from(
        std::env::var_os("FORGE_TEST_FEST_CONFIG").expect("operator Fest config required"),
    );
    assert!(path.is_absolute() && path.canonicalize().unwrap() == path);
    let expected = std::env::var("FORGE_TEST_FEST_CONFIG_SHA256").unwrap();
    assert_eq!(hash_file(&path, 1024 * 1024), expected);
    let bytes = bounded(&path, 1024 * 1024);
    assert_eq!(hash_file(&path, 1024 * 1024), expected);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        std::env::var("FORGE_TEST_FEST_CONFIG_SHA256").unwrap()
    );
    let value =
        conductor_native::mutation_evidence::parse_request(std::str::from_utf8(&bytes).unwrap())
            .unwrap();
    let config: Config = serde_json::from_value(value).unwrap();
    assert_eq!(config.schema_version, 1);
    assert!(config.programs.contains_key("python") && config.programs.contains_key("fest"));
    let guest_root = Path::new(config.python_guest["root"].as_str().unwrap());
    let guest_manifest = Path::new(config.python_guest["manifest_path"].as_str().unwrap());
    assert!(guest_root.is_absolute() && guest_root.canonicalize().unwrap() == guest_root);
    assert!(config.read_only.iter().any(|path| path == guest_root));
    assert!(
        config.cgroup_parent.is_absolute()
            && config.cgroup_parent.canonicalize().unwrap() == config.cgroup_parent
    );
    assert_eq!(
        hash_file(guest_manifest, 8 * 1024 * 1024),
        config.python_guest["manifest_sha256"].as_str().unwrap()
    );
    for path in &config.read_only {
        assert!(path.is_absolute() && path.canonicalize().unwrap() == *path);
    }
    for program in config
        .programs
        .values()
        .chain(std::iter::once(&config.setup_git))
    {
        assert!(program.path.is_absolute() && program.path.canonicalize().unwrap() == program.path);
        assert_eq!(hash_file(&program.path, 512 * 1024 * 1024), program.sha256);
    }
    (config, bytes)
}
fn put(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn command(host: &CargoFixture, config: &Config, executable: &Path) -> Command {
    let root = host.root.as_ref().unwrap().path();
    let mut command = Command::new(executable);
    command
        .current_dir(&host.repo)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PATH", config.setup_git.path.parent().unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("TMPDIR", root.join("scratch"));
    command
}
fn output(
    host: &CargoFixture,
    command: &mut Command,
    label: &str,
    seconds: u64,
) -> std::process::Output {
    host.uncertain.set(true);
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let owner = OwnedChild {
        child: Some(child),
        uncertain: host.uncertain.clone(),
        receipt: host.repo.join(RECEIPT),
    };
    let output = owner.capture(Duration::from_secs(seconds));
    let root = host.root.as_ref().unwrap().path().join("logs");
    fs::write(root.join(format!("{label}.stdout")), &output.stdout).unwrap();
    fs::write(root.join(format!("{label}.stderr")), &output.stderr).unwrap();
    fs::write(root.join(format!("{label}.command.json")), json!({"program":command.get_program().to_string_lossy(),
        "argv":command.get_args().map(|v|v.to_string_lossy()).collect::<Vec<_>>(),"returncode":output.status.code()}).to_string()).unwrap();
    output
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
        host.root.as_ref().unwrap().path().join("hooks").display()
    ))
    .args(args);
    let out = output(host, &mut cmd, label, 15);
    assert!(out.status.success(), "{label}: {out:?}");
}

fn fixture(cfg: &Config, config_bytes: &[u8], strong: bool, collection: bool) -> CargoFixture {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["repo", "scratch", "home", "logs", "hooks", "observations"] {
        private_directory(&root.path().join(name));
    }
    let repo = root.path().join("repo");
    put(&repo, "forge.toml", "schema_version=1\n[conductor]\nmutation_registry='campaigns/registry.json'\nmutation_receipt_root='campaigns/receipts'\ncandidate_policy='policy.toml'\n");
    put(&repo, ".gitignore", "/campaigns/\n");
    put(&repo, "policy.toml", POLICY);
    let source = if collection {
        COLLECTION_SOURCE
    } else {
        SOURCE
    };
    put(&repo, "subject.py", source);
    for name in SHADOW_FILES {
        put(
            &repo,
            name,
            "raise RuntimeError('repository import-shadow decoy executed')\n",
        );
    }
    let assertion = if strong {
        "    assert normal == 7 and rewritten_value == 7\n    assert rewritten.answer(1) == 1\n    with pytest.raises(AssertionError): rewritten.answer(0)\n    with pytest.raises(AssertionError): rewritten.answer(-1)\n"
    } else {
        "    assert isinstance(normal, int) and isinstance(rewritten_value, int)\n"
    };
    let test_source = if collection {
        r#"import json, os, time
import subject
if subject.answer() != 8:
    marker = 'automatic mutant changed collection input'
    row = {'marker':marker, 'source':open(subject.__file__, encoding='utf-8').read(), 'pid':os.getpid()}
    path = os.path.join(os.environ['TMPDIR'], 'forge-observation-' + str(os.getpid()) + '.json')
    with open(path + '.tmp', 'w', encoding='utf-8') as stream:
        json.dump(row, stream)
    os.replace(path + '.tmp', path)
    time.sleep(0.15)
    raise RuntimeError(marker)
def test_answer():
    assert subject.answer() == 8
"#.to_string()
    } else {
        format!("{TEST_HEAD}{assertion}")
    };
    put(&repo, "test_subject.py", &test_source);
    let registry = json!({"schema_version":1,"enforcement":"changed_tests","campaigns":[],
        "test_patterns":conductor_native::mutation_evidence::canonical_test_patterns(),
        "receipt_directories":["campaigns/receipts"]});
    put(&repo, "campaigns/registry.json", &registry.to_string());
    fs::write(root.path().join("operator-config.json"), config_bytes).unwrap();
    let programs: serde_json::Map<String, Value> = cfg
        .programs
        .iter()
        .map(|(name, pin)| (name.clone(), json!({"path":pin.path,"sha256":pin.sha256})))
        .collect();
    let grant = root.path().join("grant.json");
    let value = json!({"version":1,"source_root":repo,"scratch_parent":root.path().join("scratch"),
        "cgroup_parent":cfg.cgroup_parent,"read_only":cfg.read_only,"programs":programs,
        "resources":{"cpu_millis":1000,"memory_mib":1024,"processes":64},"max_wall_seconds":120,
        "python_guest":cfg.python_guest});
    fs::write(&grant, value.to_string()).unwrap();
    fs::set_permissions(&grant, fs::Permissions::from_mode(0o600)).unwrap();
    let pin = digest(&grant);
    let host = CargoFixture {
        root: Some(root),
        repo,
        grant,
        pin,
        uncertain: std::rc::Rc::new(std::cell::Cell::new(true)),
    };
    initialize_fixture(&host, cfg, source);
    host
}
fn initialize_fixture(host: &CargoFixture, cfg: &Config, source: &str) {
    let template = format!(
        "--template={}",
        host.root.as_ref().unwrap().path().join("hooks").display()
    );
    git(host, cfg, &["init", "-q", "-b", "main", &template], "init");
    fs::set_permissions(host.repo.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    git(
        host,
        cfg,
        &[
            "add",
            "forge.toml",
            "policy.toml",
            ".gitignore",
            "subject.py",
            "test_subject.py",
            "pytest.py",
            "coverage.py",
            "json.py",
            "pytest_cov/__init__.py",
            "src/pytest.py",
            "src/coverage.py",
            "src/json.py",
            "src/pytest_cov/__init__.py",
        ],
        "add",
    );
    git(
        host,
        cfg,
        &["add", "-f", "campaigns/registry.json"],
        "registry",
    );
    git(host, cfg, &["commit", "-qm", "base"], "base");
    git(host, cfg, &["checkout", "-qb", "fest-control"], "branch");
    put(
        &host.repo,
        "subject.py",
        &format!("{source}# scoped change\n"),
    );
    git(host, cfg, &["add", "subject.py"], "changed-add");
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
    let out = output(host, cmd.args(args), label, seconds);
    let value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{label}: {e}: {out:?}"));
    (out.status, value)
}
fn generate(host: &CargoFixture, cfg: &Config) -> (PathBuf, Value, String) {
    let argv = [
        "mutation",
        "write",
        "python",
        "--repo-root",
        host.repo.to_str().unwrap(),
        "--owner",
        "lane",
        "--base",
        "main",
        "--only",
        "subject.py",
        "--fest-confined",
        "--python-jobs",
        "1",
        "--run-timeout",
        "90",
    ];
    let (status, value) = forge(host, cfg, &argv, "generate", 30);
    assert!(status.success(), "{value}");
    let paths: Vec<_> = value["written"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| !p.contains("/registry.d/"))
        .collect();
    assert_eq!(paths.len(), 1);
    let manifest = host.repo.join(paths[0]);
    let raw = fs::read(&manifest).unwrap();
    assert!(raw.len() <= 2 * 1024 * 1024);
    let value: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(value["language"], "python");
    assert_eq!(value["mutation_engine"], "fest");
    assert_eq!(
        value["generator"]["options"]["execution_profile"],
        "fest-confined-v1"
    );
    assert_eq!(value["generator"]["source"], json!(["subject.py"]));
    assert_eq!(value["generator"]["jobs"], 1);
    assert_eq!(
        value["test_argv"],
        json!([
            "python",
            "-m",
            "pytest",
            "-q",
            "--rootdir=.",
            "test_subject.py"
        ])
    );
    assert_eq!(
        value["source_sha256"],
        json!({"subject.py":digest(&host.repo.join("subject.py"))})
    );
    assert_eq!(
        value["test_sha256"],
        json!({"test_subject.py":digest(&host.repo.join("test_subject.py"))})
    );
    let sha = format!("{:x}", Sha256::digest(&raw));
    fs::write(
        host.root
            .as_ref()
            .unwrap()
            .path()
            .join("logs/manifest-before.json"),
        raw,
    )
    .unwrap();
    (manifest, value, sha)
}
struct Watcher {
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<Result<usize, String>>>,
}
impl Watcher {
    fn start(host: &CargoFixture) -> Self {
        let root = host.root.as_ref().unwrap().path().to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let task = thread::spawn(move || collect(&root, &done));
        Self {
            stop,
            task: Some(task),
        }
    }
    fn finish(mut self) -> Result<usize, String> {
        self.stop.store(true, Ordering::Release);
        self.task
            .take()
            .unwrap()
            .join()
            .map_err(|_| "observation watcher panicked".to_string())?
    }
}
impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
fn collect(root: &Path, done: &AtomicBool) -> Result<usize, String> {
    let mut seen = BTreeSet::new();
    loop {
        for entry in fs::read_dir(root.join("scratch")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let output = entry.path().join("fest-output");
            let tmp = output.join("tmp");
            if [&output, &tmp].iter().any(|p| {
                !fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            }) {
                continue;
            }
            let Ok(files) = fs::read_dir(tmp) else {
                continue;
            };
            for file in files {
                let path = file.map_err(|e| e.to_string())?.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !name.starts_with("forge-observation-")
                    || !name.ends_with(".json")
                    || seen.contains(name)
                {
                    continue;
                }
                if seen.len() >= 4096 {
                    return Err("observation count exceeds bound".into());
                }
                let mut file = match fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                let before = file.metadata().map_err(|e| e.to_string())?;
                if !before.is_file() || before.len() > 8192 {
                    return Err("invalid observation file".into());
                }
                let mut bytes = Vec::new();
                file.by_ref()
                    .take(8193)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                let after = file.metadata().map_err(|e| e.to_string())?;
                use std::os::unix::fs::MetadataExt;
                if bytes.len() > 8192
                    || (
                        before.dev(),
                        before.ino(),
                        before.len(),
                        before.mtime(),
                        before.mtime_nsec(),
                    ) != (
                        after.dev(),
                        after.ino(),
                        after.len(),
                        after.mtime(),
                        after.mtime_nsec(),
                    )
                {
                    return Err("observation changed or exceeds bound".into());
                }
                let _: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                fs::write(root.join("observations").join(name), bytes)
                    .map_err(|e| e.to_string())?;
                seen.insert(name.to_owned());
            }
        }
        if done.load(Ordering::Acquire) {
            return Ok(seen.len());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn tracked_hashes(host: &CargoFixture, cfg: &Config, label: &str) -> BTreeMap<String, String> {
    let mut cmd = command(host, cfg, &cfg.setup_git.path);
    let out = output(host, cmd.args(["ls-files", "-z"]), label, 10);
    assert!(out.status.success());
    out.stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let name = std::str::from_utf8(path).unwrap().to_owned();
            let hash = hash_file(&host.repo.join(&name), 32 * 1024 * 1024);
            (name, hash)
        })
        .collect()
}
fn check_cleanup(host: &CargoFixture, cfg: &Config, receipt: &Value) {
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
        assert!(path.starts_with(&cfg.cgroup_parent) && path != cfg.cgroup_parent);
        assert!(absent(path), "owned cgroup remains: {}", path.display());
    }
    let snapshot = Path::new(receipt["snapshot"]["root"].as_str().unwrap());
    assert_eq!(
        snapshot.parent().unwrap(),
        host.root.as_ref().unwrap().path().join("scratch")
    );
    assert_eq!(receipt["snapshot"]["removed"], true);
    assert!(absent(snapshot));
    let cache = Path::new(receipt["fest_cache"]["path"].as_str().unwrap());
    assert_eq!(
        cache.parent().unwrap(),
        host.root.as_ref().unwrap().path().join("scratch")
    );
    assert_eq!(receipt["fest_cache"]["read_only"], true);
    assert_eq!(receipt["fest_cache"]["initially_empty"], true);
    assert_eq!(receipt["fest_cache"]["schema_version"], 1);
    assert_eq!(receipt["fest_cache"]["removed"], true);
    assert!(absent(cache));
}
fn observed(root: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(root.join("observations")).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        rows.push(serde_json::from_slice(&bytes).unwrap());
    }
    rows
}
fn altered(original: &str, mutant: &Value) -> Option<String> {
    let start = usize::try_from(mutant["byte_offset"].as_u64()?).ok()?;
    let length = usize::try_from(mutant["byte_length"].as_u64()?).ok()?;
    let end = start.checked_add(length)?;
    let old = mutant["original_text"].as_str()?;
    let new = mutant["mutated_text"].as_str()?;
    if original.as_bytes().get(start..end)? != old.as_bytes() {
        return None;
    }
    Some(format!(
        "{}{}{}",
        original.get(..start)?,
        new,
        original.get(end..)?
    ))
}
fn verify_observations(
    host: &CargoFixture,
    cfg: &Config,
    receipt: &Value,
    rows: &[Value],
    strong: bool,
) {
    let original = fs::read_to_string(host.repo.join("subject.py")).unwrap();
    let baseline: Vec<_> = rows
        .iter()
        .filter(|row| row["source"] == original)
        .collect();
    assert!(!baseline.is_empty(), "original import was not witnessed");
    for row in rows {
        let source = row["source"].as_str().unwrap();
        assert_eq!(
            row["sha256"],
            format!("{:x}", Sha256::digest(source.as_bytes()))
        );
        assert_eq!(row["size"], source.len());
        assert_eq!(row["dont_write"], true);
        assert_eq!(row["write_denied"], true);
        assert_eq!(
            row["normal"], row["rewritten"],
            "normal and rewritten imports disagree"
        );
        assert_eq!(row["normal_loader"], "SourceFileLoader");
        assert_eq!(row["rewritten_loader"], "AssertionRewritingHook");
        assert_eq!(row["prefix"], receipt["fest_cache"]["path"]);
        let guest = Path::new(cfg.python_guest["root"].as_str().unwrap());
        assert_eq!(row["guest_prefix"].as_str(), guest.to_str());
        for name in ["pytest", "pytest_cov", "coverage", "json"] {
            let origin = Path::new(row["origins"][name].as_str().expect("module origin"));
            assert!(
                origin.starts_with(guest) && origin != guest,
                "{name} not loaded from pinned guest: {}",
                origin.display()
            );
        }
    }
    let mutants = receipt["mutants"].as_array().unwrap();
    for outcome in if strong {
        &["KILLED"][..]
    } else {
        &["KILLED", "SURVIVED"][..]
    } {
        let seen = mutants
            .iter()
            .filter(|mutant| mutant["outcome"] == *outcome)
            .filter_map(|mutant| altered(&original, mutant))
            .any(|source| rows.iter().any(|row| row["source"] == source));
        assert!(
            seen,
            "inconclusive: no observed source for {outcome} outcome"
        );
    }
    let witnessed = mutants
        .iter()
        .filter_map(|mutant| altered(&original, mutant))
        .any(|mutant| {
            mutant.len() == original.len()
                && rows.iter().any(|row| {
                    row["source"] == mutant
                        && baseline
                            .iter()
                            .any(|base| base["mtime_sec"] == row["mtime_sec"])
                })
        });
    assert!(
        witnessed,
        "inconclusive: no automatically mutated same-size/same-second source was witnessed"
    );
}
fn verify_manifest(host: &CargoFixture, path: &Path, before: &Value, receipt: &Value) {
    let bytes = fs::read(path).unwrap();
    let after: Value = serde_json::from_slice(&bytes).unwrap();
    for key in [
        "campaign_id",
        "generator",
        "test_argv",
        "source_sha256",
        "test_sha256",
    ] {
        assert_eq!(after[key], before[key], "manifest {key} changed");
    }
    assert_eq!(receipt["campaign_id"], after["campaign_id"]);
    assert_eq!(
        receipt["manifest_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(receipt["source_sha256"], after["source_sha256"]);
    assert_eq!(receipt["test_sha256"], after["test_sha256"]);
    assert_eq!(
        receipt["manifest"],
        path.strip_prefix(&host.repo).unwrap().to_str().unwrap()
    );
}

fn verify_outcome(
    receipt: &Value,
    shown: &Value,
    status: std::process::ExitStatus,
    forge_hash: &str,
    strong: bool,
) {
    assert_eq!(receipt["native_runner"]["source_matches_git"], true);
    assert_eq!(receipt["native_runner"]["executable_sha256"], forge_hash);
    assert_eq!(receipt["baseline"]["returncode"], 0);
    assert_eq!(receipt["baseline"]["timed_out"], false);
    assert_eq!(receipt["complete_campaign"], true);
    let mutants = receipt["mutants"]
        .as_array()
        .expect("automatic engine rows");
    assert!(
        mutants.iter().any(|row| row["outcome"] == "KILLED"),
        "no killed mutant: {receipt}"
    );
    let kills = mutants
        .iter()
        .filter(|row| row["outcome"] == "KILLED")
        .count();
    assert_eq!(receipt["attribution"]["status"], "ATTRIBUTED");
    assert_eq!(receipt["attribution"]["attributed_mutants"], kills);
    assert_eq!(receipt["attribution"]["unattributed"], json!([]));
    assert_eq!(receipt["attribution"]["unusable_nodeids"], json!([]));
    assert!(receipt["test_value"]["tests"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["nodeid"] == "test_subject.py::test_answer"));
    if strong {
        assert!(status.success(), "strong control did not PASS: {shown}");
        assert_eq!(receipt["status"], "PASS");
        assert!(mutants.iter().all(|row| row["outcome"] == "KILLED"));
    } else {
        assert!(!status.success(), "weak control must not PASS");
        assert_ne!(receipt["status"], "PASS");
        assert!(
            mutants.iter().any(|row| row["outcome"] == "SURVIVED"),
            "weak control has no actual survivor"
        );
    }
}

fn collection_command(receipt: &Value, position: usize) -> bool {
    let suffix = format!("/mutant-{position}.xml");
    receipt["process_commands"]
        .as_array()
        .is_some_and(|commands| {
            commands.iter().any(|row| {
                row["confinement"]["outcome"] == "exited"
                    && row["confinement"]["exit_code"] == 2
                    && row["confinement"]["signal"].is_null()
                    && row["argv"].as_array().is_some_and(|args| {
                        args.iter().any(|arg| {
                            arg.as_str().is_some_and(|arg| {
                                arg.starts_with("--junitxml=") && arg.ends_with(&suffix)
                            })
                        })
                    })
            })
        })
}

fn verify_collection_error(
    host: &CargoFixture,
    receipt: &Value,
    status: std::process::ExitStatus,
    forge_hash: &str,
) {
    assert!(!status.success(), "collection-error control must not pass");
    assert_eq!(receipt["native_runner"]["executable_sha256"], forge_hash);
    assert_eq!(receipt["baseline"]["returncode"], 0);
    assert_eq!(receipt["baseline"]["timed_out"], false);
    assert_eq!(receipt["status"], "ERROR");
    assert!(receipt["error"]
        .as_str()
        .is_some_and(|error| error.contains("attribution")));
    let killed: Vec<_> = receipt["mutants"]
        .as_array()
        .expect("automatic Fest outcomes")
        .iter()
        .filter(|row| row["outcome"] == "KILLED")
        .collect();
    let unattributed = receipt["attribution"]["unattributed"]
        .as_array()
        .expect("attribution refusals");
    let original = fs::read_to_string(host.repo.join("subject.py")).unwrap();
    let observations = observed(host.root.as_ref().unwrap().path());
    // Marker bytes and command are correlated to the same mutant; the ledger
    // does not independently bind the observed marker PID to that command.
    let witnessed = killed.iter().enumerate().any(|(position, row)| {
        let rejected = unattributed.iter().any(|item| item["id"] == row["id"]);
        let source = altered(&original, row);
        let marked = observations.iter().any(|entry| {
            entry["marker"] == "automatic mutant changed collection input"
                && entry["source"].as_str() == source.as_deref()
        });
        rejected && marked && collection_command(receipt, position)
    });
    assert!(witnessed, "no generated killed mutant had a matching collection marker and attribution exit 2: {receipt}");
}

struct Notice {
    root: PathBuf,
    control: &'static str,
    state: &'static str,
}
impl Drop for Notice {
    fn drop(&mut self) {
        eprintln!(
            "{}",
            json!({"kind":"fest-fixture","root":&self.root,
            "control":self.control,"state":self.state,"scope":"owned fixture; raw evidence retained"})
        );
    }
}

fn exercise(cfg: &Config, config_bytes: &[u8], strong: bool, collection: bool) {
    let host = fixture(cfg, config_bytes, strong, collection);
    let mut notice = Notice {
        root: host.root.as_ref().unwrap().path().to_owned(),
        control: if collection {
            "collection_error"
        } else if strong {
            "strong"
        } else {
            "weak"
        },
        state: "failed_or_incomplete",
    };
    let forge_path = Path::new(forge_binary::path());
    let forge_hash = hash_file(forge_path, 512 * 1024 * 1024);
    let before = tracked_hashes(&host, cfg, "tracked-before");
    let (manifest, generated, manifest_hash) = generate(&host, cfg);
    assert_eq!(hash_file(&manifest, 2 * 1024 * 1024), manifest_hash);
    let watcher = Watcher::start(&host);
    let args = [
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
    ];
    let (status, shown) = forge(&host, cfg, &args, "run", 180);
    let seen = watcher
        .finish()
        .expect("capture bounded guest observations");
    assert!(
        seen > 0,
        "inconclusive: no guest observation captured; result={shown}"
    );
    let (show_status, receipt) = forge(
        &host,
        cfg,
        &["receipt", "show", host.repo.join(RECEIPT).to_str().unwrap()],
        "receipt",
        15,
    );
    assert!(show_status.success(), "receipt: {receipt}");
    if collection {
        verify_collection_error(&host, &receipt, status, &forge_hash);
    } else {
        verify_outcome(&receipt, &shown, status, &forge_hash, strong);
        verify_observations(
            &host,
            cfg,
            &receipt,
            &observed(host.root.as_ref().unwrap().path()),
            strong,
        );
    }
    verify_manifest(&host, &manifest, &generated, &receipt);
    check_cleanup(&host, cfg, &receipt);
    assert_eq!(tracked_hashes(&host, cfg, "tracked-after"), before);
    assert_eq!(hash_file(forge_path, 512 * 1024 * 1024), forge_hash);
    for program in cfg.programs.values().chain(std::iter::once(&cfg.setup_git)) {
        assert_eq!(hash_file(&program.path, 512 * 1024 * 1024), program.sha256);
    }
    let config_path = PathBuf::from(std::env::var_os("FORGE_TEST_FEST_CONFIG").unwrap());
    assert_eq!(
        hash_file(&config_path, 1024 * 1024),
        format!("{:x}", Sha256::digest(config_bytes))
    );
    let manifest_path = Path::new(cfg.python_guest["manifest_path"].as_str().unwrap());
    assert_eq!(
        hash_file(manifest_path, 8 * 1024 * 1024),
        cfg.python_guest["manifest_sha256"].as_str().unwrap()
    );
    host.settle(&shown, &receipt);
    assert!(!host.uncertain.get(), "fixture cleanup was not verified");
    // Retain the verified fixture and observations for independent review.
    // CargoFixture owns no live process here; its TempDir otherwise removes the evidence.
    notice.state = "completed_and_cleanup_verified";
    drop(notice);
    std::mem::forget(host);
}

#[test]
#[ignore = "requires explicit pinned FORGE_TEST_FEST_CONFIG/SHA256, FORGE_TEST_BIN and delegated cgroup with real Fest guest"]
fn real_fest_strong_and_weak_controls_witness_automatic_mutants_and_cache_isolation() {
    let (cfg, bytes) = config();
    exercise(&cfg, &bytes, true, false);
    exercise(&cfg, &bytes, false, false);
}

#[test]
#[ignore = "requires explicit pinned FORGE_TEST_FEST_CONFIG/SHA256, FORGE_TEST_BIN and delegated cgroup with real Fest guest"]
fn real_fest_collection_error_is_not_a_kill_proof() {
    let (cfg, bytes) = config();
    exercise(&cfg, &bytes, false, true);
}
