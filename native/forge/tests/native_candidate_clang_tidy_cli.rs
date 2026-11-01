#![cfg(target_os = "linux")]
#[path = "support/candidate_clang_fixture.rs"]
#[allow(dead_code)]
mod candidate_clang_fixture;
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_clang_fixture::{assert_code, commit, run, Private};
use candidate_execution_fixture::{value, Case};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
const CONFIG: &str = "Checks: '-*,clang-analyzer-core.DivideZero'\nInheritParentConfig: false\n";
// The unchanged first line intentionally has an analyzer warning. A clean
// changed second line must pass; whole-file checking would fail this control.
const BASE: &str = "int unchanged_bug(void) { int zero = 0; return 1 / zero; }\nint calculate(void) { return 8 / DIVISOR; }\n";
const SAFE: &str = "int unchanged_bug(void) { int zero = 0; return 1 / zero; }\nint calculate(void) { return 4 / DIVISOR; }\n";
const BUGGY: &str = "int unchanged_bug(void) { int zero = 0; return 1 / zero; }\nint calculate(void) { return 4 / 0; }\n";
fn database(case: &Case, configs: &[(&str, &[&str])]) -> Vec<u8> {
    let directory = case.fixture.root.join("repo").canonicalize().unwrap();
    let entries: Vec<_> = configs
        .iter()
        .map(|(file, extra)| {
            let mut arguments = vec!["cc", "-std=c11", "-c", *file];
            arguments.extend_from_slice(extra);
            json!({"directory":directory,"file":file,"arguments":arguments})
        })
        .collect();
    serde_json::to_vec(&entries).unwrap()
}
fn owned_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn operator(case: &Case) -> PathBuf {
    let path = case.fixture.root.join("operator");
    fs::create_dir_all(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn context(case: &Case, resource: &Path) -> Value {
    json!({"schema_version":1,"source_root":case.fixture.root.join("repo").canonicalize().unwrap(),
        "database":{"kind":"snapshot","path":"compile_commands.json"},"resource_dir":resource})
}
fn pin(case: &Case, document: &Value) -> (PathBuf, String) {
    let path = operator(case).join("tidy-context.json");
    let bytes = serde_json::to_vec(document).unwrap();
    owned_file(&path, &bytes);
    (path, format!("{:x}", Sha256::digest(&bytes)))
}
fn private(with_db: bool) -> (Private, Value) {
    let case = Case::new(Path::new("/bin/true"));
    let db = database(&case, &[("src/a.c", &["-DDIVISOR=2"])]);
    let mut target: Vec<(&str, &[u8])> = vec![
        ("src/a.c", SAFE.as_bytes()),
        (".clang-tidy", CONFIG.as_bytes()),
    ];
    if with_db {
        target.push(("compile_commands.json", &db));
    }
    let resource = operator(&case).join("resource");
    fs::create_dir_all(resource.join("include")).unwrap();
    fs::set_permissions(&resource, fs::Permissions::from_mode(0o700)).unwrap();
    let document = context(&case, &resource);
    let private = Private::from_case(case, &[("src/a.c", BASE.as_bytes())], &target);
    (private, document)
}
fn request(f: &Private, document: &Value, files: &[&str]) -> Command {
    let (path, hash) = pin(&f.case, document);
    let mut command = f.tool_command("clang-tidy", files);
    command
        .env("FORGE_CLANG_TIDY_CONTEXT", path)
        .env("FORGE_CLANG_TIDY_CONTEXT_SHA256", hash);
    command
}
#[test]
fn dry_plan_uses_captured_source_database_and_configuration_with_empty_path() {
    let (f, document) = private(true);
    let before = f.case.source_state();
    let repo = f.case.fixture.root.join("repo");
    for (name, dirty) in [
        ("src/a.c", "dirty source\n"),
        ("compile_commands.json", "invalid dirty DB\n"),
        (".clang-tidy", "invalid dirty config\n"),
    ] {
        fs::write(repo.join(name), dirty).unwrap();
    }
    let report = value(run(request(&f, &document, &["src/a.c"])), 0);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["executed"], false);
    assert!(report["plan"].is_object());
    assert_eq!(fs::read(repo.join("src/a.c")).unwrap(), b"dirty source\n");
    assert_eq!(
        fs::read(repo.join("compile_commands.json")).unwrap(),
        b"invalid dirty DB\n"
    );
    assert_eq!(
        fs::read(repo.join(".clang-tidy")).unwrap(),
        b"invalid dirty config\n"
    );
    assert_eq!(
        fs::read(f.snapshot.path().join("src/a.c")).unwrap(),
        SAFE.as_bytes()
    );
    f.case.assert_preserved(&before);
}
#[test]
fn absent_database_and_snapshot_source_drift_fail_without_a_tool_or_discovery() {
    let (missing, document) = private(false);
    let before = missing.case.source_state();
    let output = run(request(&missing, &document, &["src/a.c"]));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("database"));
    missing.case.assert_preserved(&before);
    let (f, document) = private(true);
    let before = f.case.source_state();
    fs::write(f.snapshot.path().join("src/a.c"), b"replaced snapshot\n").unwrap();
    assert!(!run(request(&f, &document, &["src/a.c"])).status.success());
    f.case.assert_preserved(&before);
}
#[test]
fn external_database_requires_exact_bytes_and_a_bound_source_tree() {
    let (f, mut document) = private(true);
    let before = f.case.source_state();
    let path = operator(&f.case).join("external-db.json");
    let bytes = database(&f.case, &[("src/a.c", &["-DDIVISOR=2"])]);
    owned_file(&path, &bytes);
    document["database"] = json!({"kind":"external","path":path,
        "sha256":format!("{:x}",Sha256::digest(&bytes)),"source_tree":f.tree});
    assert_code(&run(request(&f, &document, &["src/a.c"])), 0);
    owned_file(&path, b"[]");
    assert!(!run(request(&f, &document, &["src/a.c"])).status.success());
    owned_file(&path, &bytes);
    document["database"]["source_tree"] = json!("0".repeat(40));
    assert!(!run(request(&f, &document, &["src/a.c"])).status.success());
    f.case.assert_preserved(&before);
}
fn staged_case(files: &[(&str, &str)], configs: &[(&str, &[&str])]) -> Case {
    let mut case = Case::new(Path::new(support::forge_binary::path()));
    case.staged_policy(|text| text.replace("timeout_seconds=2", "timeout_seconds=30")
        .replace("memory_mb=128", "memory_mb=1024")
        .replace("\nclasses=['rust']\ncommand=", "\nclasses=['cfamily']\ncommand=")
        .replace("command=['worker','echo','literal argument','{files}']", "command=['worker','candidate','clang-tidy','--repo','{snapshot}','--base','{base}','--tree','{tree}','--execute','--','{files}']")
        .replace("version_command=['worker','echo','fixture-v1']", "version_command=['worker','candidate','clang-tidy','--tool-version']"));
    for (path, source) in files {
        case.fixture.stage(path, source);
    }
    case.fixture.stage(".clang-tidy", CONFIG);
    let db = database(&case, configs);
    case.fixture
        .stage("compile_commands.json", std::str::from_utf8(&db).unwrap());
    commit(&case);
    case.grants["resources"] = json!({"cpu_millis":2000,"memory_mib":512,"processes":32});
    case.grants["max_wall_seconds"] = json!(40);
    case.grants["max_cpu_seconds"] = json!(40);
    case.grants["max_address_space_mib"] = json!(2048);
    case
}
fn provision(case: &mut Case, header_units: Value) {
    case.delegated();
    let tool = PathBuf::from(
        std::env::var_os("FORGE_TEST_CLANG_TIDY").expect("explicit native clang-tidy required"),
    )
    .canonicalize()
    .unwrap();
    let resource = PathBuf::from(
        std::env::var_os("FORGE_TEST_CLANG_RESOURCE_DIR")
            .expect("explicit clang resource directory required"),
    )
    .canonicalize()
    .unwrap();
    assert!(resource.join("include").is_dir());
    let mut magic = [0; 4];
    fs::File::open(&tool)
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, b"\x7fELF");
    let mut document = context(case, &resource);
    document["header_units"] = header_units;
    let (path, hash) = pin(case, &document);
    case.grants["environment"] = json!({"FORGE_CLANG_TIDY":tool,
        "FORGE_CLANG_TIDY_CONTEXT":path,"FORGE_CLANG_TIDY_CONTEXT_SHA256":hash});
    let operator = operator(case);
    case.grants["read_only"].as_array_mut().unwrap().extend([
        json!(tool.parent().unwrap()),
        json!(resource),
        json!(operator),
    ]);
}
fn stage_result(case: &Case, expected: i32) -> Value {
    let before = case.source_state();
    let command = {
        let mut command = case.command();
        command.arg("--execute");
        command
    };
    let output = run(command);
    assert_code(&output, expected);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["result"]["status"],
        if expected == 0 { "passed" } else { "failed" }
    );
    let version = report["result"]["tool_version"].as_str().unwrap();
    assert!(!version.is_empty() && version != "unknown");
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    for stage in stages {
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    case.assert_preserved(&before);
    report
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_TIDY, FORGE_TEST_CLANG_RESOURCE_DIR and FORGE_TEST_CGROUP_PARENT"]
fn real_stage_clean_and_divide_zero_candidates_keep_original_source_and_database() {
    for (candidate, expected) in [(SAFE, 0), (BUGGY, 1)] {
        let mut case = staged_case(&[("src/a.c", BASE)], &[("src/a.c", &["-DDIVISOR=2"])]);
        case.fixture.stage("src/a.c", candidate);
        provision(&mut case, json!({}));
        let repo = case.fixture.root.join("repo");
        fs::write(repo.join("src/a.c"), b"dirty original must survive\n").unwrap();
        fs::write(repo.join("compile_commands.json"), b"invalid dirty DB\n").unwrap();
        let report = stage_result(&case, expected);
        assert_eq!(report["files"], json!(["src/a.c"]));
        assert_eq!(
            fs::read(repo.join("src/a.c")).unwrap(),
            b"dirty original must survive\n"
        );
        assert_eq!(
            fs::read(repo.join("compile_commands.json")).unwrap(),
            b"invalid dirty DB\n"
        );
    }
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_TIDY, FORGE_TEST_CLANG_RESOURCE_DIR and FORGE_TEST_CGROUP_PARENT"]
fn real_stage_runs_each_database_configuration_instead_of_only_the_first_or_last() {
    for definitions in [
        ["-DDIVISOR=2", "-DDIVISOR=0"],
        ["-DDIVISOR=0", "-DDIVISOR=2"],
    ] {
        let mut case = staged_case(
            &[("src/a.c", BASE)],
            &[
                ("src/a.c", &[definitions[0]]),
                ("src/a.c", &[definitions[1]]),
            ],
        );
        case.fixture.stage("src/a.c", SAFE);
        provision(&mut case, json!({}));
        let report = stage_result(&case, 1);
        let command = &report["stages"][1]["report"];
        assert!(
            format!("{}{}", command["stdout"], command["stderr"]).contains("zero"),
            "{report}"
        );
    }
}
#[test]
#[ignore = "requires FORGE_TEST_CLANG_TIDY, FORGE_TEST_CLANG_RESOURCE_DIR and FORGE_TEST_CGROUP_PARENT"]
fn real_stage_header_units_distinguish_two_same_basename_translation_units() {
    let header = "static inline int calculate(void) { return 8 / DIVISOR; }\n";
    for (units, expected, missing_inclusion) in [
        (json!(["left/unit.c"]), 0, false),
        (json!(["left/unit.c", "right/unit.c"]), 1, false),
        (json!(["left/unit.c", "right/unit.c"]), 1, true),
    ] {
        let right = if missing_inclusion {
            "int right(void) { return 0; }\n"
        } else {
            "#include \"../include/a.h\"\nint right(void) { return calculate(); }\n"
        };
        let mut case = staged_case(
            &[
                ("include/a.h", header),
                (
                    "left/unit.c",
                    "#include \"../include/a.h\"\nint left(void) { return calculate(); }\n",
                ),
                ("right/unit.c", right),
            ],
            &[
                ("left/unit.c", &["-DDIVISOR=2"]),
                ("right/unit.c", &["-DDIVISOR=0"]),
            ],
        );
        case.fixture.stage(
            "include/a.h",
            "static inline int calculate(void) { return 4 / DIVISOR; }\n",
        );
        provision(&mut case, json!({"include/a.h":units}));
        let report = stage_result(&case, expected);
        assert_eq!(report["files"], json!(["include/a.h"]));
        if missing_inclusion {
            let stderr = report["stages"][1]["report"]["stderr"].as_str().unwrap();
            assert!(stderr.contains("include coverage incomplete"), "{stderr}");
        }
    }
}

#[test]
#[ignore = "requires FORGE_TEST_CLANG_TIDY, FORGE_TEST_CLANG_RESOURCE_DIR and FORGE_TEST_CGROUP_PARENT"]
fn real_stage_reports_selected_system_header_diagnostics() {
    for (header, expected) in [
        ("#define COMPUTE(x) ((x) * (x) + 1)\n", 0),
        ("#define COMPUTE(x) x * x + 1\n", 1),
    ] {
        let mut case = staged_case(
            &[
                ("include/a.h", "#define COMPUTE(x) ((x) * (x))\n"),
                (
                    "src/unit.c",
                    "#include <a.h>\nint calculate(void) { return COMPUTE(2); }\n",
                ),
            ],
            &[("src/unit.c", &["-isystem", "include"])],
        );
        case.fixture.stage(".clang-tidy", "Checks: '-*,bugprone-macro-parentheses'\nExcludeHeaderFilterRegex: '.*'\nInheritParentConfig: false\n");
        commit(&case);
        case.fixture.stage("include/a.h", header);
        provision(&mut case, json!({"include/a.h":["src/unit.c"]}));
        let report = stage_result(&case, expected);
        if expected != 0 {
            assert!(
                format!("{}", report["stages"][1]["report"]).contains("bugprone-macro-parentheses")
            );
        }
    }
}
