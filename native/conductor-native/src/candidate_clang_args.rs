//! Bounded, filesystem-independent compilation command admission and rebasing.
//! Paths in `inputs` still require confined, identity-bound filesystem admission.
//! The operator compiler is a virtual clang-tidy driver identity; it is not
//! executed or evidence of an installed compiler binary.
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

const MAX_ARGS: usize = 4096;
const MAX_ARG_BYTES: usize = 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_MAPS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Source,
    Directory,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub path: PathBuf,
    pub kind: InputKind,
    pub mapping: MappingKind,
}

#[derive(Clone, Debug)]
pub struct Mapping {
    pub original: PathBuf,
    pub admitted: PathBuf,
    pub kind: MappingKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingKind {
    Source,
    External,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub arguments: Vec<String>,
    pub inputs: Vec<Input>,
}

fn fail<T>(message: &str) -> Result<T, String> {
    Err(message.into())
}

fn clean(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_PATH {
        return fail("path must be absolute and at most 4096 bytes");
    }
    let mut result = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => result.push(name),
            Component::ParentDir => {
                if !result.pop() {
                    return fail("path traverses above root");
                }
            }
            Component::Prefix(_) => return fail("unsupported path prefix"),
        }
    }
    Ok(result)
}

fn text(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("non-UTF-8 path")?;
    if value.bytes().any(|b| b.is_ascii_control()) {
        return fail("control byte in path");
    }
    Ok(value.to_owned())
}

fn absolute(base: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.len() > MAX_PATH || raw.bytes().any(|b| b.is_ascii_control()) {
        return fail("invalid path operand");
    }
    let path = Path::new(raw);
    clean(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

fn mapping_check(maps: &[Mapping]) -> Result<(), String> {
    if maps.is_empty() || maps.len() > MAX_MAPS {
        return fail("mapping count outside bounds");
    }
    let mut originals = BTreeSet::new();
    let sources: Vec<_> = maps
        .iter()
        .filter(|map| map.kind == MappingKind::Source)
        .collect();
    if sources.len() != 1 {
        return fail("exactly one source mapping is required");
    }
    for map in maps {
        if clean(&map.original)? != map.original || clean(&map.admitted)? != map.admitted {
            return fail("mapping roots must be normalized absolute paths");
        }
        text(&map.original)?;
        text(&map.admitted)?;
        if !originals.insert(&map.original) {
            return fail("duplicate original mapping root");
        }
        if map.admitted.starts_with(&sources[0].original) {
            return fail("mapped output remains under original source root");
        }
    }
    Ok(())
}

fn map(path: &Path, maps: &[Mapping]) -> Result<(PathBuf, MappingKind), String> {
    let selected = maps
        .iter()
        .filter(|m| path.starts_with(&m.original))
        .max_by_key(|m| m.original.components().count())
        .ok_or("path is outside declared mappings")?;
    let suffix = path
        .strip_prefix(&selected.original)
        .map_err(|_| "invalid mapped prefix")?;
    let value = clean(&selected.admitted.join(suffix))?;
    text(&value)?;
    Ok((value, selected.kind))
}

fn arg_input(
    raw: &str,
    base: &Path,
    maps: &[Mapping],
    kind: InputKind,
    inputs: &mut Vec<Input>,
) -> Result<String, String> {
    let (path, mapping) = map(&absolute(base, raw)?, maps)?;
    let value = text(&path)?;
    inputs.push(Input {
        path,
        kind,
        mapping,
    });
    Ok(value)
}

fn safe_value(s: &str) -> bool {
    !s.is_empty() && !s.bytes().any(|b| b.is_ascii_control())
}

fn validate(input: &[String], compiler: &str) -> Result<(), String> {
    if input.is_empty() || input.len() > MAX_ARGS || !safe_value(&input[0]) {
        return fail("invalid compile argv");
    }
    let mut total = 0usize;
    for arg in input {
        total = total
            .checked_add(arg.len())
            .ok_or("compile argv overflow")?;
        if total > MAX_ARG_BYTES || arg.bytes().any(|b| b.is_ascii_control()) {
            return fail("compile argv exceeds bounds or contains control bytes");
        }
    }
    let path = Path::new(compiler);
    if !matches!(compiler, "clang" | "clang++") && (clean(path)? != path || !safe_value(compiler)) {
        return fail("operator driver must be clang, clang++ or a normalized absolute driver path");
    }
    if driver_kind(&input[0])? != driver_kind(compiler)? {
        return fail("DB driver and operator driver language differ");
    }
    Ok(())
}

fn driver_kind(path: &str) -> Result<bool, String> {
    match Path::new(path).file_name().and_then(|v| v.to_str()) {
        Some("clang" | "gcc" | "cc") => Ok(false),
        Some("clang++" | "g++" | "c++") => Ok(true),
        _ => fail("unsupported compiler driver; explicitly provision a matching driver"),
    }
}

/// Clang tooling consumes this driver name; no database executable is launched.
pub fn virtual_driver(driver: &str) -> Result<&'static str, String> {
    Ok(if driver_kind(driver)? {
        "clang++"
    } else {
        "clang"
    })
}

/// Resolve a database translation unit lexically without opening the old host.
pub fn source_path(directory: &str, file: &str) -> Result<PathBuf, String> {
    if !Path::new(directory).is_absolute() {
        return fail("compile directory must be absolute");
    }
    absolute(&absolute(Path::new("/"), directory)?, file)
}

struct State<'a> {
    base: PathBuf,
    source_original: PathBuf,
    maps: &'a [Mapping],
    output: Vec<String>,
    inputs: Vec<Input>,
    source_count: usize,
}

fn path_arg(state: &mut State<'_>, flag: &str, raw: &str, kind: InputKind) -> Result<(), String> {
    let value = arg_input(raw, &state.base, state.maps, kind, &mut state.inputs)?;
    state.output.push(flag.to_owned());
    state.output.push(value);
    Ok(())
}

fn pair<'a>(args: &'a [String], i: &mut usize) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .filter(|x| safe_value(x))
        .ok_or("missing option operand".into())
}

fn safe_simple(arg: &str) -> bool {
    matches!(
        arg,
        "-fPIC"
            | "-fpic"
            | "-fPIE"
            | "-fpie"
            | "-fno-exceptions"
            | "-fexceptions"
            | "-fno-rtti"
            | "-frtti"
            | "-fno-omit-frame-pointer"
            | "-pthread"
            | "-nostdinc"
            | "-nostdinc++"
            | "-pedantic"
            | "-pedantic-errors"
    ) || [
        "-g", "-g0", "-g1", "-g2", "-g3", "-O0", "-O1", "-O2", "-O3", "-Os", "-Oz", "-Og", "-Ofast",
    ]
    .contains(&arg)
        || (arg.starts_with("-W")
            && arg.len() > 2
            && !arg.starts_with("-Wl,")
            && !arg.starts_with("-Wp,")
            && !arg.starts_with("-Wa,")
            && arg
                .bytes()
                .skip(2)
                .all(|b| b.is_ascii_alphanumeric() || b"-_=+".contains(&b)))
}

fn safe_atom(s: &str) -> bool {
    safe_value(s) && s.len() <= MAX_PATH && !s.starts_with('-') && !s.contains('@')
}

fn safe_literal(value: &str, maps: &[Mapping]) -> bool {
    maps.iter()
        .filter(|m| m.kind == MappingKind::Source)
        .all(|m| {
            m.original
                .to_str()
                .is_some_and(|root| !value.contains(root))
        })
}

fn source_operand(state: &mut State<'_>, raw: &str) -> Result<(), String> {
    if absolute(&state.base, raw)? != state.source_original {
        return fail("unexpected positional compile operand");
    }
    state.source_count += 1;
    if state.source_count != 1 {
        return fail("duplicate source operand");
    }
    let value = arg_input(
        raw,
        &state.base,
        state.maps,
        InputKind::Source,
        &mut state.inputs,
    )?;
    state.output.push(value);
    Ok(())
}

fn path_option(arg: &str) -> Option<(&'static str, &str, InputKind)> {
    if arg == "--sysroot" {
        return Some(("--sysroot", "", InputKind::Directory));
    }
    for (prefix, flag, kind) in [
        ("-isystem", "-isystem", InputKind::Directory),
        ("-iquote", "-iquote", InputKind::Directory),
        ("-imacros", "-imacros", InputKind::File),
        ("-include", "-include", InputKind::File),
        ("--sysroot=", "--sysroot", InputKind::Directory),
        ("-isysroot", "-isysroot", InputKind::Directory),
        ("-I", "-I", InputKind::Directory),
    ] {
        if let Some(value) = arg.strip_prefix(prefix) {
            return Some((flag, value, kind));
        }
    }
    None
}

fn one(state: &mut State<'_>, args: &[String], i: &mut usize) -> Result<(), String> {
    let arg = &args[*i];
    if matches!(arg.as_str(), "-c" | "-MMD" | "-MD" | "-MP") {
        return Ok(());
    }
    if matches!(arg.as_str(), "-o" | "-MF" | "-MT" | "-MQ") {
        pair(args, i)?;
        return Ok(());
    }
    if arg.starts_with('@')
        || arg.starts_with("-include-pch")
        || arg.starts_with("-include-pth")
        || arg.starts_with("-X")
        || arg.starts_with("-fplugin")
        || arg.starts_with("-fmodule")
        || arg.starts_with("-ivfsoverlay")
        || arg.starts_with("--config")
    {
        return fail("unsupported or unsafe compile option");
    }
    if let Some((flag, value, kind)) = path_option(arg) {
        let value = if value.is_empty() {
            pair(args, i)?
        } else {
            value
        };
        if value.starts_with('-') || value.starts_with('=') {
            return fail("unsupported include/sysroot operand");
        }
        return path_arg(state, flag, value, kind);
    }
    if ["-D", "-U", "-x", "-target", "--target", "-std"].contains(&arg.as_str()) {
        let value = pair(args, i)?;
        if !safe_atom(value) || !safe_literal(value, state.maps) {
            return fail("invalid compile option value");
        }
        state.output.push(arg.clone());
        state.output.push(value.into());
        return Ok(());
    }
    if (["-D", "-U", "-std=", "--target="]
        .iter()
        .any(|p| arg.strip_prefix(p).is_some_and(safe_atom))
        || safe_simple(arg))
        && safe_literal(arg, state.maps)
    {
        state.output.push(arg.clone());
        return Ok(());
    }
    if arg.starts_with('-') {
        return fail("unsupported compile option");
    }
    source_operand(state, arg)
}

/// Rebase a single compile command; does not open or execute anything.
pub fn rebase(
    directory: &str,
    file: &str,
    arguments: &[String],
    mappings: &[Mapping],
    compiler: &str,
) -> Result<Command, String> {
    mapping_check(mappings)?;
    validate(arguments, compiler)?;
    if !Path::new(directory).is_absolute() {
        return fail("compile directory must be absolute");
    }
    let base = absolute(Path::new("/"), directory)?;
    let source_original = absolute(&base, file)?;
    let source_mapping = mappings
        .iter()
        .find(|m| m.kind == MappingKind::Source)
        .expect("validated source mapping");
    if !source_original.starts_with(&source_mapping.original) {
        return fail("translation unit is outside original source root");
    }
    let (directory, directory_mapping) = map(&base, mappings)?;
    let (target_file, source_mapping_kind) = map(&source_original, mappings)?;
    if source_mapping_kind != MappingKind::Source
        || !target_file.starts_with(&source_mapping.admitted)
    {
        return fail("translation unit mapped outside snapshot source root");
    }
    let mut state = State {
        base,
        source_original,
        maps: mappings,
        output: vec![compiler.into()],
        inputs: vec![Input {
            path: directory.clone(),
            kind: InputKind::Directory,
            mapping: directory_mapping,
        }],
        source_count: 0,
    };
    let mut i = 1;
    let mut positional = false;
    while i < arguments.len() {
        if arguments[i] == "--" && !positional {
            positional = true;
            i += 1;
            continue;
        }
        if positional {
            source_operand(&mut state, &arguments[i])?;
        } else {
            one(&mut state, arguments, &mut i)?;
        }
        i += 1;
    }
    if state.source_count != 1 {
        return fail("compile command must contain exactly one source operand");
    }
    let bytes = state
        .output
        .iter()
        .try_fold(0usize, |sum, arg| sum.checked_add(arg.len()))
        .ok_or("rebased argv overflow")?;
    if bytes > MAX_ARG_BYTES {
        return fail("rebased argv exceeds byte limit");
    }
    Ok(Command {
        directory,
        file: target_file,
        arguments: state.output,
        inputs: state.inputs,
    })
}

#[cfg(test)]
#[path = "candidate_clang_args_tests.rs"]
mod tests;
