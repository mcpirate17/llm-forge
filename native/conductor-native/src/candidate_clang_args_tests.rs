use super::*;

fn maps() -> Vec<Mapping> {
    vec![
        Mapping {
            original: "/repo".into(),
            admitted: "/snapshot".into(),
            kind: MappingKind::Source,
        },
        Mapping {
            original: "/build".into(),
            admitted: "/sealed-build".into(),
            kind: MappingKind::External,
        },
        Mapping {
            original: "/toolchain".into(),
            admitted: "/toolchain".into(),
            kind: MappingKind::External,
        },
    ]
}
fn run(args: &[&str]) -> Result<Command, String> {
    rebase(
        "/build/obj",
        "../../repo/src/a file.cc",
        &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
        &maps(),
        "/toolchain/bin/clang++",
    )
}

#[test]
fn joined_and_separate_sysroots_are_rebased_identically() {
    let first = run(&[
        "c++",
        "--sysroot=/toolchain/sdk",
        "../../repo/src/a file.cc",
    ])
    .unwrap();
    let second = run(&[
        "c++",
        "--sysroot",
        "/toolchain/sdk",
        "../../repo/src/a file.cc",
    ])
    .unwrap();
    assert_eq!(first.arguments, second.arguments);
    assert!(first
        .inputs
        .iter()
        .any(|i| i.path == Path::new("/toolchain/sdk") && i.kind == InputKind::Directory));
}

#[test]
fn cmake_flags_are_rebased_and_build_outputs_removed() {
    let result = run(&[
        "/usr/bin/c++",
        "-I../../repo/include",
        "-isystem",
        "../generated",
        "-DVALUE=hello world",
        "-std=c++20",
        "-Wall",
        "-O2",
        "-MD",
        "-MF",
        "a.d",
        "-c",
        "../../repo/src/a file.cc",
        "-o",
        "a.o",
    ])
    .unwrap();
    assert_eq!(result.directory, PathBuf::from("/sealed-build/obj"));
    assert_eq!(result.file, PathBuf::from("/snapshot/src/a file.cc"));
    assert_eq!(
        result.arguments,
        [
            "/toolchain/bin/clang++",
            "-I",
            "/snapshot/include",
            "-isystem",
            "/sealed-build/generated",
            "-DVALUE=hello world",
            "-std=c++20",
            "-Wall",
            "-O2",
            "/snapshot/src/a file.cc"
        ]
    );
    assert!(result
        .inputs
        .iter()
        .any(|i| i.path == Path::new("/snapshot/include") && i.kind == InputKind::Directory));
}

#[test]
fn unsafe_flags_and_unknown_inputs_fail_closed() {
    for flag in [
        "@args.rsp",
        "-Xclang",
        "-fplugin=/tmp/p.so",
        "-fmodules",
        "-include-pch",
        "-ivfsoverlay",
        "--config=/tmp/c",
        "-Wp,-include,/tmp/x",
        "-funknown",
    ] {
        assert!(
            run(&["clang++", flag, "../../repo/src/a file.cc"]).is_err(),
            "{flag}"
        );
    }
    assert!(run(&["clang++", "-I/elsewhere", "../../repo/src/a file.cc"]).is_err());
    assert!(run(&["clang++", "../../repo/src/a file.cc", "another.cc"]).is_err());
    assert!(run(&[
        "clang++",
        "../../repo/src/a file.cc",
        "../../repo/src/a file.cc"
    ])
    .is_err());
}

#[test]
fn component_prefix_and_nested_mapping_are_unambiguous() {
    let mut m = maps();
    m.push(Mapping {
        original: "/repo/generated".into(),
        admitted: "/sealed-generated".into(),
        kind: MappingKind::External,
    });
    let args = [
        "cc",
        "-I../../repo/generated/include",
        "../../repo/src/a file.cc",
    ]
    .map(str::to_owned);
    let result = rebase(
        "/build/obj",
        "../../repo/src/a file.cc",
        &args,
        &m,
        "/toolchain/bin/clang",
    )
    .unwrap();
    assert!(result
        .arguments
        .contains(&"/sealed-generated/include".into()));
    assert!(map(Path::new("/repository/file"), &m).is_err());
    m.push(m[0].clone());
    assert!(mapping_check(&m).is_err());
}

#[test]
fn input_bounds_and_mapping_back_to_source_refuse() {
    let mut m = maps();
    m[0].admitted = "/repo/snapshot".into();
    assert!(mapping_check(&m).is_err());
    assert!(run(&[
        "clang++",
        "-I",
        "../../../../outside",
        "../../repo/src/a file.cc"
    ])
    .is_err());
    assert!(run(&[
        "clang++",
        "-include",
        "../../repo/a\nfile",
        "../../repo/src/a file.cc"
    ])
    .is_err());
    assert!(run(&[
        "clang++",
        &"x".repeat(MAX_ARG_BYTES + 1),
        "../../repo/src/a file.cc"
    ])
    .is_err());
    assert!(run(&["clang++", "-DROOT=/repo", "../../repo/src/a file.cc"]).is_err());
    assert!(run(&["cc", "../../repo/src/a file.cc"]).is_err());
}

#[test]
fn virtual_driver_name_requires_matching_language() {
    let args = ["c++".into(), "../../repo/src/a file.cc".into()];
    assert!(rebase(
        "/build/obj",
        "../../repo/src/a file.cc",
        &args,
        &maps(),
        "clang++"
    )
    .is_ok());
    assert!(rebase(
        "/build/obj",
        "../../repo/src/a file.cc",
        &args,
        &maps(),
        "clang"
    )
    .is_err());
}
