mod c_inventory_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{c_test_fn_name, inventory_c_test_nodeids};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    fn write_source(body: &str) -> (PathBuf, String) {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("llm-c-inventory-{}-{serial}", std::process::id()));
        let relative = "tests/test_subject.c";
        fs::create_dir_all(root.join("tests")).expect("create tree");
        fs::write(root.join(relative), body).expect("write source");
        (root, relative.to_owned())
    }

    #[test]
    fn inventory_lists_c_tests_in_source_order_and_skips_callbacks() {
        // The callback is the real shape from test_profiler.c: a `test_`
        // prefixed helper that ctest never registers, so counting it would make
        // a complete scope claim a case that cannot run.
        let (root, relative) = write_source(
            "#include <assert.h>\n\
             static void test_beta(void) { assert(1); }\n\
             static void test_sink_fn(const evt_t* e, void* u) { (void)e; (void)u; }\n\
             static void test_alpha(void) { assert(1); }\n\
             int main(void) { return 0; }\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                "tests/test_subject.c::test_beta".to_owned(),
                "tests/test_subject.c::test_alpha".to_owned(),
            ]
        );
    }

    #[test]
    fn inventory_skips_a_forward_declaration() {
        // `static void test_x(void);` compiles and registers nothing.
        let (root, relative) = write_source(
            "static void test_later(void);\n\
             static void test_now(void) { }\n\
             static void test_later(void) { }\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                "tests/test_subject.c::test_now".to_owned(),
                "tests/test_subject.c::test_later".to_owned(),
            ]
        );
    }

    #[test]
    fn inventory_ignores_an_indented_definition() {
        // The governance inventory and the CMake registration must read the
        // same set. CMake anchors at column zero; anything else is a case ctest
        // will not run, and claiming it in a complete scope gates nothing.
        let (root, relative) = write_source(
            "static void test_real(void) { }\n#if 0\n    static void test_hidden(void) { }\n#endif\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(found, vec!["tests/test_subject.c::test_real".to_owned()]);
    }

    #[test]
    fn inventory_refuses_a_duplicate_c_test_name() {
        let (root, relative) =
            write_source("static void test_reset(void) { }\nstatic void test_reset(void) { }\n");
        let error = inventory_c_test_nodeids(&root, &relative)
            .expect_err("ctest names are flat, so two cases would be indistinguishable");
        assert!(error.contains("duplicate test name"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_c_file_with_no_tests() {
        let (root, relative) = write_source("int main(void) { return 0; }\n");
        let error = inventory_c_test_nodeids(&root, &relative)
            .expect_err("an empty complete scope would gate nothing");
        assert!(
            error.contains("complete C test scope is empty"),
            "got {error}"
        );
    }

    #[test]
    fn inventory_refuses_a_missing_c_file() {
        let (root, _) = write_source("static void test_x(void) { }\n");
        let error = inventory_c_test_nodeids(&root, "tests/absent.c")
            .expect_err("a missing file must not read as an empty inventory");
        assert!(error.contains("cannot inventory C tests"), "got {error}");
    }

    #[test]
    fn fn_name_requires_the_void_signature_and_a_body() {
        assert_eq!(
            c_test_fn_name("static void test_ok(void) {"),
            Some("test_ok".to_owned())
        );
        assert_eq!(c_test_fn_name("static void test_ok(void);"), None);
        assert_eq!(c_test_fn_name("static void test_cb(int x) {"), None);
        assert_eq!(c_test_fn_name("static void helper(void) {"), None);
        assert_eq!(c_test_fn_name("void test_ok(void) {"), None);
    }
}
