use super::*;
fn headers(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}
#[test]
fn absolute_relative_parent_and_duplicate_headers_match_exact_paths() {
    let required = headers(&[
        "/snapshot/include/a.h",
        "/snapshot/include/雪.h",
        "/snapshot/include/a.h",
    ]);
    let stderr = b"normal warning\n. ../include/a.h\n.. /snapshot/include/\xe9\x9b\xaa.h\r\n. ../include/a.h\n";
    assert!(verify(stderr, Path::new("/snapshot/build"), &required).is_ok());
}
#[test]
fn duplicate_basenames_require_each_distinct_header() {
    let required = headers(&["/snapshot/left/common.h", "/snapshot/right/common.h"]);
    let error = verify(
        b". /snapshot/left/common.h\n",
        Path::new("/snapshot"),
        &required,
    )
    .unwrap_err();
    assert!(error.to_string().contains("coverage incomplete: 1"));
    assert!(verify(
        b". left/common.h\n.. right/common.h\n",
        Path::new("/snapshot"),
        &required
    )
    .is_ok());
}
#[test]
fn normal_diagnostics_and_nontrace_spoofs_never_satisfy_coverage() {
    let required = headers(&["/snapshot/include/a.h"]);
    for diagnostic in [
        "file.c:2: warning: . /snapshot/include/a.h\n",
        "note: ../include/a.h\n",
        " ... /snapshot/include/a.h\n",
        ".\t/snapshot/include/a.h\n",
        ".../snapshot/include/a.h\n",
        ".  /snapshot/include/a.h\n",
        ". /snapshot/include/a.h trailing diagnostic\n",
        ". ../other/a.h\n",
        ".. this is diagnostic text\n",
    ] {
        let error = verify(
            diagnostic.as_bytes(),
            Path::new("/snapshot/build"),
            &required,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("coverage incomplete"),
            "{diagnostic:?}: {error}"
        );
    }
}
#[test]
fn missing_header_and_invalid_trace_encoding_fail_explicitly() {
    let required = headers(&["/snapshot/include/a.h"]);
    assert!(verify(b"", Path::new("/snapshot"), &required)
        .unwrap_err()
        .to_string()
        .contains("coverage incomplete"));
    for trace in [
        &b". /snapshot/include/\xff.h\n"[..],
        &b"bad diagnostic \xff\n"[..],
        &b". /snapshot/include/a.h\0\n"[..],
        &b". \n"[..],
    ] {
        assert!(verify(trace, Path::new("/snapshot"), &required).is_err());
    }
}
#[test]
fn empty_obligation_does_not_require_or_invent_trace_evidence() {
    assert!(verify(b"ordinary diagnostic only\n", Path::new("/snapshot"), &[]).is_ok());
    assert!(verify(b"\xff", Path::new("/snapshot"), &[]).is_ok());
    assert!(verify(b"", Path::new("relative"), &[]).is_err());
}
#[test]
fn admission_limits_and_required_path_validation_are_independent_of_matches() {
    let required = headers(&["/snapshot/a.h"]);
    assert!(verify(&vec![b' '; MAX_TRACE + 1], Path::new("/snapshot"), &[]).is_err());
    assert!(verify(&vec![b'x'; MAX_LINE + 1], Path::new("/snapshot"), &required).is_err());
    assert!(verify(&vec![b'x'; MAX_LINE + 1], Path::new("/snapshot"), &[]).is_err());
    let deep = format!("{} /snapshot/a.h\n", ".".repeat(MAX_DEPTH));
    assert!(verify(deep.as_bytes(), Path::new("/snapshot"), &required).is_ok());
    assert!(verify(
        format!(".{deep}").as_bytes(),
        Path::new("/snapshot"),
        &required
    )
    .is_err());
    assert!(verify(
        b"",
        Path::new("/snapshot"),
        &vec![PathBuf::from("/snapshot/a.h"); MAX_HEADERS + 1]
    )
    .is_err());
    for invalid in [
        "relative.h",
        "/snapshot/../outside.h",
        "/snapshot/line\nbreak.h",
    ] {
        assert!(verify(b"", Path::new("/snapshot"), &headers(&[invalid])).is_err());
    }
    assert!(verify(
        b". /snapshot/a.h\n. bad\0path\n",
        Path::new("/snapshot"),
        &required
    )
    .is_err());
}
fn base_filter(entries: &[(&str, Vec<[u32; 2]>)]) -> String {
    serde_json::to_string(
        &entries
            .iter()
            .map(|(name, lines)| serde_json::json!({"name":name,"lines":lines}))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn entries(filter: &str) -> Vec<FilterEntry> {
    serde_json::from_str(filter).unwrap()
}
#[test]
fn expansion_copies_only_the_canonical_header_ranges_to_exact_spellings() {
    let required = headers(&["/snapshot/include/a.h"]);
    let base = base_filter(&[
        ("/snapshot/src/main.c", vec![[30, 31]]),
        ("/snapshot/include/a.h", vec![[2, 4], [9, 9]]),
    ]);
    let trace =
        b". /snapshot/left/../include/a.h\n.. ../include/./a.h\n. /snapshot/left/../include/a.h\n";
    let expanded = expanded_filter(trace, Path::new("/snapshot/build"), &required, &base).unwrap();
    let parsed = entries(&expanded);
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[0].name, "/snapshot/src/main.c");
    assert_eq!(parsed[0].lines, [[30, 31]]);
    assert_eq!(parsed[1].name, "/snapshot/include/a.h");
    let names: Vec<_> = parsed[2..]
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "/snapshot/build/../include/./a.h",
            "/snapshot/left/../include/a.h"
        ]
    );
    assert!(parsed[1..]
        .iter()
        .all(|entry| entry.lines == [[2, 4], [9, 9]]));
}
#[test]
fn string_aliases_keep_dot_and_repeated_separator_spellings_distinct() {
    let required = headers(&["/snapshot/include/a.h"]);
    let base = base_filter(&[("/snapshot/include/a.h", vec![[1, 1]])]);
    let trace = b". /snapshot/./include/a.h\n. /snapshot//include/a.h\n.. /snapshot/include/a.h\n";
    let expanded = expanded_filter(trace, Path::new("/snapshot"), &required, &base).unwrap();
    let names: Vec<_> = entries(&expanded)
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        names,
        [
            "/snapshot/include/a.h",
            "/snapshot/./include/a.h",
            "/snapshot//include/a.h"
        ]
    );
    let reverse = b". /snapshot/include/a.h\n. /snapshot//include/a.h\n. /snapshot/./include/a.h\n";
    assert_eq!(
        expanded_filter(reverse, Path::new("/snapshot"), &required, &base).unwrap(),
        expanded
    );
}
#[test]
fn duplicate_basenames_copy_independent_ranges_and_unrequired_traces_add_nothing() {
    let required = headers(&["/snapshot/left/a.h", "/snapshot/right/a.h"]);
    let base = base_filter(&[
        ("/snapshot/left/a.h", vec![[2, 2]]),
        ("/snapshot/right/a.h", vec![[8, 9]]),
    ]);
    let trace = b". left/./a.h\n. right/./a.h\n. other/./a.h\n";
    let expanded = expanded_filter(trace, Path::new("/snapshot"), &required, &base).unwrap();
    let parsed = entries(&expanded);
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[2].name, "/snapshot/left/./a.h");
    assert_eq!(parsed[2].lines, [[2, 2]]);
    assert_eq!(parsed[3].name, "/snapshot/right/./a.h");
    assert_eq!(parsed[3].lines, [[8, 9]]);
}
#[test]
fn unchanged_filter_is_byte_preserved_and_missing_obligations_fail() {
    let base = "[ { \"name\": \"/snapshot/a.h\", \"lines\": [[3,4]] } ]";
    let required = headers(&["/snapshot/a.h"]);
    assert_eq!(
        expanded_filter(
            b". /snapshot/a.h\n",
            Path::new("/snapshot"),
            &required,
            base
        )
        .unwrap(),
        base
    );
    assert_eq!(
        expanded_filter(b"\xff", Path::new("/snapshot"), &[], base).unwrap(),
        base
    );
    assert!(expanded_filter(
        b"warning: . /snapshot/a.h\n",
        Path::new("/snapshot"),
        &required,
        base
    )
    .unwrap_err()
    .to_string()
    .contains("coverage incomplete"));
    assert!(expanded_filter(
        b". /snapshot/a.h\n",
        Path::new("/snapshot"),
        &required,
        "[]"
    )
    .unwrap_err()
    .to_string()
    .contains("canonical header has no filter ranges"));
}
#[test]
fn filter_schema_and_encoded_output_admission_precede_amplified_range_cloning() {
    let required = headers(&["/snapshot/a.h"]);
    let base = base_filter(&[("/snapshot/a.h", vec![[1, 1]; 4096])]);
    assert!(base.len() < MAX_FILTER / 2);
    let trace = b". /snapshot/left/../a.h\n. /snapshot/./a.h\n";
    assert!(
        expanded_filter(trace, Path::new("/snapshot"), &required, &base)
            .unwrap_err()
            .to_string()
            .contains("byte limit")
    );
    let exact = format!("{}[]", " ".repeat(MAX_FILTER - 2));
    assert_eq!(
        expanded_filter(b"", Path::new("/snapshot"), &[], &exact).unwrap(),
        exact
    );
    assert!(expanded_filter(b"", Path::new("/snapshot"), &[], &format!(" {exact}")).is_err());
    for bad in [
        r#"[{"name":"/snapshot/a.h","name":"/snapshot/a.h","lines":[[1,2]]}]"#,
        r#"[{"name":"/snapshot/a.h","lines":[[0,2]]}]"#,
        r#"[{"name":"/snapshot/a.h","lines":[[2,1]]}]"#,
        r#"[{"name":"/snapshot/a.h","lines":[[1,2]],"extra":true}]"#,
    ] {
        assert!(expanded_filter(b"", Path::new("/snapshot"), &[], bad).is_err());
    }
}
#[test]
fn quoted_unicode_alias_names_are_json_escaped_without_losing_ranges() {
    let required = headers(&["/snapshot/雪\".h"]);
    let base = base_filter(&[("/snapshot/雪\".h", vec![[5, 6]])]);
    let trace = ". /snapshot/./雪\".h\n";
    let expanded =
        expanded_filter(trace.as_bytes(), Path::new("/snapshot"), &required, &base).unwrap();
    let parsed = entries(&expanded);
    assert_eq!(parsed[1].name, "/snapshot/./雪\".h");
    assert_eq!(parsed[1].lines, [[5, 6]]);
    assert!(expanded.len() <= MAX_FILTER);
}
