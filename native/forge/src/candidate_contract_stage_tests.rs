use super::*;
use crate::{
    candidate_command::PreparedArgv,
    candidate_stage_artifact::{ArtifactBudget, Capture},
    cgroup::Cgroup,
    confined_execution::{self, Limits, Outcome},
};
use std::sync::Arc;
const NATIVE_MANIFEST: &str = "[package]\nname='conductor-native'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='conductor_native'\ncrate-type=['cdylib','rlib']\n[features]\npython=[]\npython-compat-tests=['python']\n";
const SLOP_MANIFEST: &str = "[package]\nname='slop-core'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='slop_core'\ncrate-type=['cdylib','rlib']\n[features]\nextension-module=[]\n";
const FORGE_MANIFEST: &str = "[package]\nname='forge'\nversion='0.1.0'\nedition='2021'\n";
const CONTRACT: &str = r#"#[test] fn imports_candidate_extensions_and_forge() {
 let python=std::env::var("PYO3_PYTHON").unwrap();
 assert!(std::process::Command::new(python).args(["-c","import conductor_native, slop_core; assert conductor_native.__name__ == 'conductor_native'; assert slop_core.__name__ == 'slop_core'"]).status().unwrap().success());
 let forge=std::env::var("FORGE_TEST_BIN").unwrap();
 assert!(std::process::Command::new(forge).arg("--version").status().unwrap().success());
}"#;
fn module(name: &str) -> String {
    r#"use std::ffi::{c_char,c_void};
#[repr(C)] struct Object { refs:isize, ty:*mut c_void }
#[repr(C)] struct Base { object:Object, init:*mut c_void, index:isize, copy:*mut c_void }
#[repr(C)] struct Definition { base:Base, name:*const c_char, doc:*const c_char, size:isize, methods:*mut c_void, slots:*mut c_void, traverse:*mut c_void, clear:*mut c_void, free:*mut c_void }
static mut DEF:Definition=Definition { base:Base { object:Object {refs:1,ty:std::ptr::null_mut()},init:std::ptr::null_mut(),index:0,copy:std::ptr::null_mut() }, name:b"$NAME\0".as_ptr().cast(),doc:std::ptr::null(),size:-1,methods:std::ptr::null_mut(),slots:std::ptr::null_mut(),traverse:std::ptr::null_mut(),clear:std::ptr::null_mut(),free:std::ptr::null_mut() };
extern "C" { fn PyModule_Create2(def:*mut Definition,version:i32)->*mut c_void; }
#[no_mangle] pub unsafe extern "C" fn PyInit_$NAME()->*mut c_void { PyModule_Create2(std::ptr::addr_of_mut!(DEF),1013) }
"#.replace("$NAME",name)
}
fn lock(name: &str) -> String {
    format!("version = 4\n[[package]]\nname = '{name}'\nversion = '0.1.0'\n")
}
fn write(root: &Path, path: &str, content: &str) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, content).unwrap();
}
fn candidate(f: &Fixture, with_slop: bool) -> (String, Vec<u8>) {
    let mut files = vec![
        (
            "native/conductor-native/Cargo.toml",
            NATIVE_MANIFEST.to_owned(),
        ),
        (
            "native/conductor-native/Cargo.lock",
            lock("conductor-native"),
        ),
        (
            "native/conductor-native/src/lib.rs",
            module("conductor_native"),
        ),
        (
            "native/conductor-native/tests/python_contracts_sample.rs",
            CONTRACT.into(),
        ),
        ("native/slop-core/Cargo.toml", SLOP_MANIFEST.into()),
        ("native/slop-core/Cargo.lock", lock("slop-core")),
        ("native/slop-core/src/lib.rs", module("slop_core")),
        ("native/forge/Cargo.toml", FORGE_MANIFEST.into()),
        ("native/forge/Cargo.lock", lock("forge")),
        (
            "native/forge/src/main.rs",
            "fn main(){println!(\"candidate fixture\");}\n".into(),
        ),
    ];
    if !with_slop {
        files.retain(|(path, _)| !path.starts_with("native/slop-core/"));
        let row = files
            .iter_mut()
            .find(|(path, _)| path.ends_with("python_contracts_sample.rs"))
            .unwrap();
        row.1=CONTRACT.replace("import conductor_native, slop_core; assert conductor_native.__name__ == 'conductor_native'; assert slop_core.__name__ == 'slop_core'", "import conductor_native; assert conductor_native.__name__ == 'conductor_native'");
    }
    let repository = git2::Repository::open(&f.host).unwrap();
    let mut index = repository.index().unwrap();
    for (path, content) in &files {
        write(&f.host, path, content);
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    let before = fs::read(f.host.join(".git/index")).unwrap();
    (index.write_tree().unwrap().to_string(), before)
}
fn grants(f: &Fixture, parent: &str, forge: &str, cargo: &str, python: &str) -> Grants {
    let mut value = f.value();
    value["cgroup_parent"] = json!(parent);
    value["resources"]["memory_mib"] = json!(2048);
    // LLVM's linker uses a host-sized thread pool in addition to Cargo's workers.
    value["resources"]["processes"] = json!(128);
    value["read_only"] = json!([
        f.host,
        "/usr",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        forge,
        Path::new(cargo).parent().unwrap(),
        Path::new(python).parent().unwrap().parent().unwrap()
    ]);
    value["programs"] = json!({"forge":forge,"python":python});
    value["python"] = json!("python");
    value["environment"] = json!({"FORGE_CARGO_BIN":cargo,"FORGE_CARGO_EXTRA_PATH":"/usr/bin"});
    if let Ok(libdir) = std::env::var("FORGE_TEST_PYTHON_LIB_DIR") {
        value["read_only"]
            .as_array_mut()
            .unwrap()
            .push(json!(libdir));
        value["environment"]["FORGE_CONTRACT_PYTHON_LIB_DIR"] = json!(libdir);
    }
    f.write_config(value);
    f.grants()
}
fn argv(root: &Path, tree: String, python: String) -> Vec<String> {
    vec![
        "forge".into(),
        "candidate".into(),
        "contract-test".into(),
        "--repo".into(),
        root.to_str().unwrap().into(),
        "--base".into(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
        "--tree".into(),
        tree,
        "--python".into(),
        python,
        "--target=python_contracts_sample".into(),
        "--execute".into(),
    ]
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicit delegated cgroup, FORGE_TEST_BIN, FORGE_TEST_CARGO_BIN and FORGE_TEST_PYTHON"]
async fn real_stage_builds_candidate_extensions_and_executes_selected_contract() {
    let parent = std::env::var("FORGE_TEST_CGROUP_PARENT").expect("delegated cgroup");
    let forge = std::env::var("FORGE_TEST_BIN").expect("forge binary");
    let cargo = std::env::var("FORGE_TEST_CARGO_BIN").expect("Cargo bin");
    let python = std::env::var("FORGE_TEST_PYTHON").expect("Python binary");
    for with_slop in [true, false] {
        let f = Fixture::new();
        let (tree, index_before) = candidate(&f, with_slop);
        let grants = grants(&f, &parent, &forge, &cargo, &python);
        let workspace = Workspace::new(&f.source(), &tree, &grants).unwrap();
        let scratch = workspace.scratch.path.clone();
        let root = workspace.path();
        let command = workspace
            .plan(
                &PreparedArgv {
                    argv: argv(&root, tree, python.clone()),
                    changed_files: None,
                },
                &grants,
            )
            .unwrap();
        let (capture, receiver) = Capture::new(
            workspace,
            "contract-evidence.json",
            1024 * 1024,
            Arc::new(ArtifactBudget::new(16 * 1024 * 1024)),
        )
        .unwrap();
        let group = Cgroup::create(&grants.cgroup_parent, grants.resources).unwrap();
        let group_path = group.report().path.clone();
        let limits = Limits {
            timeout: std::time::Duration::from_secs(120),
            max_output_chars: 12000,
            cpu_seconds: Some(120),
            address_space_bytes: None,
        };
        let execution =
            confined_execution::start_with_resources(command, group, limits, Box::new(capture))
                .unwrap();
        let report = tokio::time::timeout(std::time::Duration::from_secs(150), execution.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.outcome, Outcome::Exited, "{report:?}");
        assert_eq!(report.exit_code, Some(0), "{report:?}");
        assert!(report.cleanup_complete);
        let artifact = receiver.await.unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&artifact.bytes).unwrap();
        assert_eq!(value["targets"], json!(["python_contracts_sample"]));
        assert_eq!(
            value["artifacts"].as_array().unwrap().len(),
            if with_slop { 3 } else { 2 }
        );
        assert_eq!(value["slop_candidate_built"], with_slop);
        assert_eq!(fs::read(f.host.join(".git/index")).unwrap(), index_before);
        assert!(!scratch.exists());
        assert!(!group_path.exists());
    }
}
