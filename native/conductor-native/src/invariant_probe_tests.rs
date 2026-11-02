use super::*;
#[test]
fn mapped_paths_preserve_spaces_and_deleted_suffix() {
    let line = "7f00-7f10 r-xp 0000 08:01 9 /owned/repo/a b/native.so (deleted)";
    assert_eq!(mapped_path(line), Some("/owned/repo/a b/native.so"));
    assert!(mapped_path("short").is_none());
}
#[test]
fn repo_mappings_are_not_hidden_by_site_package_names() {
    let root = Path::new("/owned/repo");
    let source = Path::new("/source/repo");
    assert_eq!(
        classified(root, source, "/owned/repo/site-packages/own.so.1")
            .unwrap()
            .as_deref(),
        Some("site-packages/own.so.1")
    );
    assert_eq!(
        classified(root, source, "/outside/site-packages/torch.so").unwrap(),
        None
    );
    assert_eq!(
        classified(root, source, "/outside/torch_extensions/build/ops.so")
            .unwrap()
            .as_deref(),
        Some("compiled-cache::ops.so")
    );
    assert_eq!(
        classified(root, source, "/owned/repo-extra/lib.so").unwrap(),
        None
    );
    assert_eq!(
        classified(root, source, "/source/repo/native/ext.so")
            .unwrap()
            .as_deref(),
        Some("source-root::native/ext.so")
    );
    assert!(classified(root, source, "/owned/repo/bad\n.so").is_err());
}
