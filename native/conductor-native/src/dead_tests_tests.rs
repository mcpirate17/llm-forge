use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    dependency_closure, scan_dead_test_module, untracked_import_closure_scan, DeadTestModule,
    DeadTestScanError, PythonModuleResolver, UntrackedImportFinding,
};

static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(0);

fn repo_path() -> PathBuf {
    let serial = NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "conductor-native-dead-tests-{}-{serial}",
        std::process::id()
    ))
}

fn write(root: &std::path::Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture has a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn untracked_import_closure_is_transitive_and_preserves_immediate_importers() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/tracked.py", "VALUE = 1\n");
    write(&root, "pkg/nested/item.py", "VALUE = 2\n");
    write(
        &root,
        "pkg/entry.py",
        r#"from pkg import tracked
def guarded():
    from pkg import untracked
try:
    from . import sibling
except ImportError:
    pass
import pkg.native
import pkg.nested
"#,
    );
    write(&root, "pkg/untracked.py", "from pkg import deeper\n");
    write(&root, "pkg/deeper.py", "VALUE = 3\n");
    write(&root, "pkg/sibling.py", "from pkg import deeper\n");
    write(&root, "pkg/broken.py", "from pkg import hidden\nif:\n");
    write(&root, "pkg/hidden.py", "VALUE = 4\n");
    write(&root, "pkg/native.rs", "pub fn marker() {}\n");
    let tracked = vec![
        "pkg/__init__.py".to_owned(),
        "pkg/broken.py".to_owned(),
        "pkg/entry.py".to_owned(),
        "pkg/native.rs".to_owned(),
        "pkg/nested/item.py".to_owned(),
        "pkg/tracked.py".to_owned(),
    ];

    let scan = untracked_import_closure_scan(&root, &tracked);

    assert_eq!(
        scan.records,
        vec![
            UntrackedImportFinding {
                module: "pkg/deeper.py".to_owned(),
                importers: vec!["pkg/sibling.py".to_owned(), "pkg/untracked.py".to_owned(),],
                tracked_importers: vec![],
            },
            UntrackedImportFinding {
                module: "pkg/sibling.py".to_owned(),
                importers: vec!["pkg/entry.py".to_owned()],
                tracked_importers: vec!["pkg/entry.py".to_owned()],
            },
            UntrackedImportFinding {
                module: "pkg/untracked.py".to_owned(),
                importers: vec!["pkg/entry.py".to_owned()],
                tracked_importers: vec!["pkg/entry.py".to_owned()],
            },
        ]
    );
    assert_eq!(scan.parse_error_paths, vec!["pkg/broken.py"]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_test_scanner_preserves_guard_relative_string_and_native_semantics() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/dep.py", "VALUE = 1\n");
    write(&root, "pkg/dynamic.py", "VALUE = 2\n");
    write(&root, "pkg/native.rs", "pub fn marker() {}\n");
    write(
        &root,
        "pkg/sub/module.py",
        r#"from typing import TYPE_CHECKING
from .. import dep
import pkg.native
if FLAG:
    import pkg.hard_missing
if TYPE_CHECKING:
    import pkg.type_missing
if typing.TYPE_CHECKING:
    import pkg.attribute_missing
if __name__ == "__main__":
    import pkg.main_missing
try:
    import pkg.try_missing
except ImportError:
    pass
def lazy():
    import pkg.lazy_missing
async def async_lazy():
    import pkg.async_missing
"pkg.dynamic"
"loader.py"
"#,
    );
    let tracked = vec![
        "pkg/__init__.py".to_owned(),
        "pkg/dep.py".to_owned(),
        "pkg/dynamic.py".to_owned(),
        "pkg/native.rs".to_owned(),
        "pkg/sub/module.py".to_owned(),
    ];
    let resolver = PythonModuleResolver::new(&root, &tracked);

    let module = scan_dead_test_module(&root, "pkg/sub/module.py", &resolver).unwrap();

    assert!(module.has_main);
    assert_eq!(module.basenames, BTreeSet::from(["loader.py".to_owned()]));
    assert_eq!(
        module.deps,
        BTreeSet::from(["pkg/dep.py".to_owned(), "pkg/dynamic.py".to_owned()])
    );
    assert_eq!(
        module.missing,
        BTreeSet::from(["pkg.hard_missing".to_owned()])
    );
    assert_eq!(
        module.soft_missing,
        BTreeSet::from([
            "pkg.async_missing".to_owned(),
            "pkg.attribute_missing".to_owned(),
            "pkg.lazy_missing".to_owned(),
            "pkg.main_missing".to_owned(),
            "pkg.try_missing".to_owned(),
            "pkg.type_missing".to_owned(),
        ])
    );
    assert!(module.untracked.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_test_closure_handles_ten_thousand_module_cycle() {
    let count = 10_000;
    let mut modules = BTreeMap::new();
    for index in 0..count {
        let path = format!("pkg/module_{index:05}.py");
        let dependency = format!("pkg/module_{:05}.py", (index + 1) % count);
        modules.insert(
            path.clone(),
            DeadTestModule {
                path,
                has_main: false,
                basenames: BTreeSet::new(),
                deps: BTreeSet::from([dependency]),
                missing: if index == count - 1 {
                    BTreeSet::from(["pkg.gone".to_owned()])
                } else {
                    BTreeSet::new()
                },
                soft_missing: BTreeSet::new(),
                untracked: if index == count / 2 {
                    BTreeSet::from(["pkg/local_only.py".to_owned()])
                } else {
                    BTreeSet::new()
                },
            },
        );
    }

    let (missing, untracked) = dependency_closure("pkg/module_00000.py", &modules).unwrap();

    assert_eq!(missing, BTreeSet::from(["pkg.gone".to_owned()]));
    assert_eq!(untracked, BTreeSet::from(["pkg/local_only.py".to_owned()]));
}

#[test]
fn resolver_scopes_untracked_files_to_its_explicit_root() {
    let target = repo_path();
    let decoy = repo_path();
    write(&decoy, "pkg/helper.py", "VALUE = 1\n");
    let resolver = PythonModuleResolver::new(&target, &["main.py".to_owned()]);
    assert_eq!(resolver.resolve_untracked("pkg.helper", "main.py"), None);
    write(&target, "pkg/helper.py", "VALUE = 1\n");
    assert_eq!(
        resolver.resolve_untracked("pkg.helper", "main.py"),
        Some("pkg/helper.py".to_owned())
    );
    fs::remove_dir_all(target).unwrap();
    fs::remove_dir_all(decoy).unwrap();
}

#[test]
fn parser_error_names_file_and_does_not_make_an_empty_module() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/broken.py", "if:\n");
    let resolver = PythonModuleResolver::new(
        &root,
        &["pkg/__init__.py".to_owned(), "pkg/broken.py".to_owned()],
    );
    match scan_dead_test_module(&root, "pkg/broken.py", &resolver) {
        Err(DeadTestScanError::Parse { path, detail }) => {
            assert_eq!(path, "pkg/broken.py");
            assert!(!detail.is_empty());
        }
        _ => panic!("invalid source must produce a parse error"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn closure_missing_dependency_reports_the_missing_key() {
    let modules = BTreeMap::from([(
        "test_probe.py".to_owned(),
        DeadTestModule {
            path: "test_probe.py".to_owned(),
            has_main: false,
            basenames: BTreeSet::new(),
            deps: BTreeSet::from(["pkg/absent.py".to_owned()]),
            missing: BTreeSet::new(),
            soft_missing: BTreeSet::new(),
            untracked: BTreeSet::new(),
        },
    )]);
    assert_eq!(
        dependency_closure("test_probe.py", &modules).unwrap_err(),
        "pkg/absent.py"
    );
}

#[test]
fn scanned_module_json_orders_dependencies_independently_of_tracked_input() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/a.py", "import pkg.c\nimport pkg.b\n");
    write(&root, "pkg/b.py", "VALUE = 1\n");
    write(&root, "pkg/c.py", "VALUE = 2\n");
    let resolver = PythonModuleResolver::new(
        &root,
        &[
            "pkg/c.py".to_owned(),
            "pkg/a.py".to_owned(),
            "pkg/b.py".to_owned(),
            "pkg/__init__.py".to_owned(),
        ],
    );
    let first = scan_dead_test_module(&root, "pkg/a.py", &resolver).unwrap();
    let second = scan_dead_test_module(&root, "pkg/a.py", &resolver).unwrap();
    let first_json = serde_json::to_string(&first).unwrap();
    assert_eq!(first_json, serde_json::to_string(&second).unwrap());
    assert_eq!(
        serde_json::to_value(&first).unwrap()["deps"],
        serde_json::json!(["pkg/b.py", "pkg/c.py"])
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_bounded_closure_preserves_transitive_importers_without_python() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/entry.py", "from pkg import hidden\n");
    write(&root, "pkg/hidden.py", "from pkg import deeper\n");
    write(&root, "pkg/deeper.py", "value = 1\n");
    let tracked = vec!["pkg/__init__.py".into(), "pkg/entry.py".into()];
    let result = super::bounded::scan(&root, &tracked).unwrap();
    assert_eq!(
        result
            .records
            .iter()
            .map(|r| r.module.as_str())
            .collect::<Vec<_>>(),
        vec!["pkg/deeper.py", "pkg/hidden.py"]
    );
    assert_eq!(result.records[0].importers, vec!["pkg/hidden.py"]);
    assert_eq!(result.records[1].tracked_importers, vec!["pkg/entry.py"]);
    assert_eq!(result.files_scanned, 4);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn native_bounded_closure_rejects_parse_failure_symlink_and_invalid_paths() {
    use std::os::unix::fs::symlink;
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/entry.py", "def broken(\n");
    let tracked = vec!["pkg/__init__.py".into(), "pkg/entry.py".into()];
    assert!(super::bounded::scan(&root, &tracked).is_err());
    write(&root, "pkg/entry.py", "import pkg.other\n");
    let outside = root.parent().unwrap().join(format!(
        "{}-outside.py",
        root.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&outside, "value = 1\n").unwrap();
    symlink(&outside, root.join("pkg/other.py")).unwrap();
    assert!(super::bounded::scan(&root, &tracked).is_err());
    assert!(super::bounded::scan(&root, &["../outside.py".into()]).is_err());
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn native_bounded_closure_rejects_oversized_source() {
    let root = repo_path();
    write(&root, "pkg/entry.py", "");
    fs::OpenOptions::new()
        .write(true)
        .open(root.join("pkg/entry.py"))
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert!(super::bounded::scan(&root, &["pkg/entry.py".into()]).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn native_closure_replay_binds_empty_sources_and_negative_import_lookups() {
    let root = repo_path();
    write(&root, "pkg/__init__.py", "");
    write(&root, "pkg/main.py", "from pkg import absent\n");
    let tracked = vec!["pkg/__init__.py".into(), "pkg/main.py".into()];
    let first = super::bounded::scan(&root, &tracked).unwrap();
    assert!(!first.sources["pkg/__init__.py"].is_empty());
    assert_eq!(first.lookups.get("pkg/absent.py"), Some(&false));
    write(&root, "pkg/absent.py", "answer = 1\n");
    let second = super::bounded::scan(&root, &tracked).unwrap();
    assert_ne!(first, second);
    assert_eq!(second.records[0].module, "pkg/absent.py");
    fs::remove_dir_all(root).unwrap();
}
