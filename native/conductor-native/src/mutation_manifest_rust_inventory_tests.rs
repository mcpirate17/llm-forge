mod rust_inventory_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{inventory_rust_test_nodeids, rust_fn_name};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    fn write_source(body: &str) -> (PathBuf, String) {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "llm-rust-inventory-{}-{serial}",
            std::process::id()
        ));
        let relative = "src/subject.rs";
        fs::create_dir_all(root.join("src")).expect("create tree");
        fs::write(root.join(relative), body).expect("write source");
        (root, relative.to_owned())
    }

    fn inventory_with_timeout(root: &Path, relative: &str) -> Result<Vec<String>, String> {
        let (done, receive) = mpsc::channel();
        let root = root.to_path_buf();
        let relative = relative.to_owned();
        std::thread::spawn(move || {
            done.send(inventory_rust_test_nodeids(&root, &relative))
                .expect("send inventory result");
        });
        receive
            .recv_timeout(Duration::from_secs(5))
            .expect("Rust inventory must terminate")
    }

    #[test]
    fn inventory_lists_tests_in_source_order_and_skips_helpers() {
        let (root, relative) = write_source(
            "#[cfg(test)]\nmod inner {\n    fn helper() {}\n\n    #[test]\n    fn beta() {}\n\n    /// doc\n    #[test]\n    #[ignore]\n    // why this test exists\n    pub fn alpha() {}\n\n    #[tokio::test]\n\n    async fn gamma() {}\n}\n",
        );
        let found = inventory_with_timeout(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                format!("{relative}::beta"),
                format!("{relative}::alpha"),
                format!("{relative}::gamma"),
            ],
            "source order is the contract; helpers and doc comments are not tests"
        );
    }

    #[test]
    fn inventory_refuses_a_name_repeated_across_modules() {
        let (root, relative) = write_source(
            "mod a {\n    #[test]\n    fn same() {}\n}\nmod b {\n    #[test]\n    fn same() {}\n}\n",
        );
        let error = inventory_with_timeout(&root, &relative)
            .expect_err("cargo_test nodeids carry no module path, so this is ambiguous");
        assert!(error.contains("duplicate test name"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_file_with_no_tests() {
        let (root, relative) = write_source("fn not_a_test() {}\n");
        let error = inventory_with_timeout(&root, &relative)
            .expect_err("an empty complete scope must fail loud");
        assert!(
            error.contains("complete Rust test scope is empty"),
            "got {error}"
        );
    }

    #[test]
    fn inventory_refuses_a_dangling_test_attribute() {
        let (root, relative) = write_source("#[test]\n");
        let error = inventory_with_timeout(&root, &relative)
            .expect_err("an attribute with no fn is malformed, not empty");
        assert!(error.contains("has no fn"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_missing_file() {
        let (root, _) = write_source("#[test]\nfn a() {}\n");
        let error = inventory_with_timeout(&root, "src/absent.rs")
            .expect_err("a missing file must not read as an empty inventory");
        assert!(error.contains("cannot inventory Rust tests"), "got {error}");
    }

    #[test]
    fn inventory_reads_attributes_that_carry_arguments() {
        let (root, relative) = write_source(
            "#[tokio::test(flavor = \"multi_thread\")]\nasync fn with_args() {}\n\n#[tokio::test(\n    flavor = \"multi_thread\",\n    worker_threads = 2,\n)]\nasync fn across_lines() {}\n",
        );
        let found = inventory_with_timeout(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                format!("{relative}::with_args"),
                format!("{relative}::across_lines"),
            ],
            "an attribute path is the text before its arguments, on one line or many"
        );
    }

    #[test]
    fn inventory_refuses_a_test_framework_it_cannot_expand() {
        for attribute in ["#[rstest]", "#[test_case(1, 2)]", "#[proptest]"] {
            let (root, relative) = write_source(&format!(
                "#[test]\nfn real() {{}}\n\n{attribute}\nfn other() {{}}\n"
            ));
            let error = inventory_with_timeout(&root, &relative)
                .expect_err("guessing would silently undercount a complete scope");
            assert!(error.contains("cannot expand"), "{attribute}: got {error}");
        }
    }

    #[test]
    fn inventory_is_not_confused_by_test_shaped_non_markers() {
        let (root, relative) = write_source(
            "#[cfg(test)]\nmod inner {\n    #[cfg_attr(test, derive(Debug))]\n    struct S;\n\n    #[test]\n    #[should_panic(expected = \"boom [test]\")]\n    fn only_one() {}\n}\n",
        );
        let found = inventory_with_timeout(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![format!("{relative}::only_one")],
            "cfg(test), cfg_attr(test, ..) and a bracket inside a string are not test markers"
        );
    }

    #[test]
    fn attribute_reader_reports_the_path_and_its_last_line() {
        let lines = [
            "#[tokio::test(",
            "    flavor = \"x\",",
            ")]",
            "async fn a() {}",
        ];
        let (path, end) = super::read_rust_attribute(&lines, 0).expect("balanced attribute");
        assert_eq!(path, "tokio::test");
        assert_eq!(end, 2, "the fn is found after the attribute's last line");
        assert_eq!(super::read_rust_attribute(&["#[tokio::test("], 0), None);
        assert_eq!(
            super::read_rust_attribute(&["#[test(thing = \"[bracket]\\\"\")]"], 0),
            Some(("test".to_owned(), 0))
        );
        assert_eq!(super::read_rust_attribute(&["not an attribute"], 0), None);
        assert_eq!(
            super::read_rust_attribute(&[r##"#[test(value = "[")]"##], 0),
            Some(("test".to_owned(), 0)),
            "brackets inside strings must not change attribute depth"
        );
    }

    #[test]
    fn attribute_reader_terminates_on_bounded_malformed_inputs() {
        let (done, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let lines = [
                "#[test(",
                "  value = \"[unterminated\"",
                "fn not_reached() {}",
            ];
            let result = super::read_rust_attribute(&lines, 0);
            done.send(result).expect("send parser result");
        });
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(5))
                .expect("malformed attribute parser must terminate"),
            None
        );
    }

    #[test]
    fn rust_inventory_terminates_on_bounded_malformed_attributes() {
        let (root, relative) =
            write_source("#[test(\n  value = \"[unterminated\"\nfn not_reached() {}\n");
        let (done, receive) = mpsc::channel();
        let root_for_thread = root.clone();
        let relative_for_thread = relative.clone();
        std::thread::spawn(move || {
            done.send(inventory_rust_test_nodeids(
                &root_for_thread,
                &relative_for_thread,
            ))
            .expect("send inventory result");
        });
        let result = receive
            .recv_timeout(Duration::from_secs(5))
            .expect("Rust inventory must terminate on malformed input");
        assert!(result.is_err(), "malformed inventory must fail closed");
    }

    #[test]
    fn attribute_classification_splits_markers_from_frameworks() {
        use super::{classify_rust_attribute, TestAttribute};
        assert_eq!(classify_rust_attribute("test"), TestAttribute::Marks);
        assert_eq!(classify_rust_attribute("tokio::test"), TestAttribute::Marks);
        assert_eq!(
            classify_rust_attribute("rstest"),
            TestAttribute::Unrecognised
        );
        assert_eq!(
            classify_rust_attribute("test_case"),
            TestAttribute::Unrecognised
        );
        assert_eq!(classify_rust_attribute("cfg"), TestAttribute::Other);
        assert_eq!(
            classify_rust_attribute("should_panic"),
            TestAttribute::Other
        );
        assert_eq!(
            classify_rust_attribute("serial_test::serial"),
            TestAttribute::Other
        );
    }

    #[test]
    fn fn_name_strips_visibility_and_qualifiers() {
        assert_eq!(rust_fn_name("fn plain() {}").as_deref(), Some("plain"));
        assert_eq!(
            rust_fn_name("pub fn exported() {}").as_deref(),
            Some("exported")
        );
        assert_eq!(
            rust_fn_name("pub(crate) async fn scoped() {}").as_deref(),
            Some("scoped")
        );
        assert_eq!(
            rust_fn_name("fn generic<T: Copy>(value: T) {}").as_deref(),
            Some("generic")
        );
        assert_eq!(rust_fn_name("let fn_like = 1;"), None);
        assert_eq!(rust_fn_name("struct NotAFn;"), None);
        // `fn ` must be the whole prefix, not merely present somewhere on the
        // line. Matching it by substring would name a function after text that
        // only mentions one, and the reader would then inventory a test that
        // does not exist and certify a complete scope around it.
        assert_eq!(rust_fn_name("let s = \"fn phantom\";"), None);
        assert_eq!(rust_fn_name("// call fn helper() later"), None);
    }
}
