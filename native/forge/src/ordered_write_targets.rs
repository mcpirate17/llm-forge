//! Ordered shell write resolution for the claim gate. The legacy raw target
//! extractor stays unchanged; this walker binds each write to its own cwd.

use super::*;
use std::collections::{HashSet, VecDeque};

const MAX_DEPTH: usize = 8;
const MAX_STATES: usize = 32;
const BOUNDARIES: &[&str] = &[";", "&&", "||", "|", "&", "\n"];

static WRITE_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:rm|mv|cp|install|ln|rsync|truncate|touch|shred|unlink|tee|dd|patch|sed|python[0-9.]*|perl|ruby|node|php|bash|sh|zsh|dash|ksh)\b|\bgit\s+(?:checkout|restore|apply|stash)\b").unwrap()
});
static CWD_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:cd|pushd|popd|source)\b").unwrap());

#[derive(Clone, Hash, Eq, PartialEq)]
struct State {
    cwd: Option<PathBuf>,
    success: bool,
}

struct Segment {
    argv: Vec<String>,
    before: Option<String>,
    bodies: Vec<String>,
}

fn segments(tokens: &[String], bodies: Vec<(String, String)>) -> Vec<Segment> {
    let mut queue: VecDeque<String> = bodies.into_iter().map(|(_, body)| body).collect();
    let mut result = Vec::new();
    let mut argv = Vec::new();
    let mut before = None;
    for token in tokens {
        if BOUNDARIES.contains(&token.as_str()) {
            if !argv.is_empty() {
                result.push(segment(
                    std::mem::take(&mut argv),
                    before.take(),
                    &mut queue,
                ));
            }
            before = Some(token.clone());
        } else {
            argv.push(token.clone());
        }
    }
    if !argv.is_empty() {
        result.push(segment(argv, before, &mut queue));
    }
    if !queue.is_empty() {
        result.push(Segment {
            argv: vec![OPAQUE_WRITE.to_string()],
            before: None,
            bodies: Vec::new(),
        });
    }
    result
}

fn segment(argv: Vec<String>, before: Option<String>, queue: &mut VecDeque<String>) -> Segment {
    let mut bodies = Vec::new();
    for token in &argv {
        if matches!(token.as_str(), "<<" | "<<-") {
            if let Some(body) = queue.pop_front() {
                bodies.push(body);
            }
        }
    }
    Segment {
        argv,
        before,
        bodies,
    }
}

fn emit(
    target: &str,
    state: &State,
    bindings: &HashMap<String, String>,
    loops: &HashMap<String, Vec<String>>,
    out: &mut Vec<String>,
) {
    for expanded in expand_all(target, bindings, loops) {
        if NULL_SINKS.contains(&expanded.as_str()) {
            continue;
        }
        if expanded == OPAQUE_WRITE || expanded.starts_with('~') {
            // shlex has discarded quoting, so a tilde may expand to a home
            // directory outside the command cwd. Do not guess its target.
            if expanded.starts_with('~') {
                out.push(OPAQUE_WRITE.to_string());
                continue;
            }
            out.push(expanded);
        } else {
            let path = Path::new(&expanded);
            match (path.is_absolute(), &state.cwd) {
                (true, _) => out.push(expanded),
                (false, Some(cwd)) => out.push(cwd.join(path).to_string_lossy().into_owned()),
                (false, None) => out.push(OPAQUE_WRITE.to_string()),
            }
        }
    }
}

fn both_statuses(state: &State) -> Vec<State> {
    [true, false]
        .into_iter()
        .map(|success| State {
            cwd: state.cwd.clone(),
            success,
        })
        .collect()
}

fn change_dir(argv: &[String], state: &State, bindings: &HashMap<String, String>) -> Vec<State> {
    let args = operands(&argv[1..], "");
    let target = args.first().map(|raw| expand(raw, bindings));
    let cwd = match target.as_deref() {
        Some(value) if value != OPAQUE_WRITE && value != "-" && !value.starts_with('~') => {
            let path = Path::new(value);
            if path.is_absolute() {
                Some(path.to_path_buf())
            } else if !value.starts_with("./")
                && !value.starts_with("../")
                && value != "."
                && value != ".."
                && (bindings.get("CDPATH").is_some_and(|v| !v.is_empty())
                    || std::env::var("CDPATH").is_ok_and(|v| !v.is_empty()))
            {
                None
            } else {
                state.cwd.as_ref().map(|base| base.join(path))
            }
        }
        _ => None,
    };
    vec![
        State { cwd, success: true },
        State {
            cwd: state.cwd.clone(),
            success: false,
        },
    ]
}

fn shell_script(args: &[String]) -> Option<&str> {
    args.iter().enumerate().find_map(|(i, arg)| {
        (arg == "-c" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c')))
            .then(|| args.get(i + 1).map(String::as_str))
            .flatten()
    })
}

fn shell_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn env_command(
    argv: &[String],
    state: &State,
    bindings: &HashMap<String, String>,
    out: &mut Vec<String>,
) -> Option<(Vec<String>, State)> {
    let mut index = 1;
    let mut cwd = state.cwd.clone();
    let mut cdpath = false;
    while let Some(arg) = argv.get(index) {
        if arg == "-C" || arg == "--chdir" {
            let Some(dir) = argv.get(index + 1) else {
                out.push(OPAQUE_WRITE.to_string());
                return None;
            };
            let expanded = expand(dir, bindings);
            if expanded == OPAQUE_WRITE || expanded.starts_with('~') {
                out.push(OPAQUE_WRITE.to_string());
                return None;
            }
            let path = Path::new(&expanded);
            cwd = if path.is_absolute() {
                Some(path.to_path_buf())
            } else {
                cwd.as_ref().map(|base| base.join(path))
            };
            index += 2;
        } else if let Some(dir) = arg.strip_prefix("--chdir=") {
            let expanded = expand(dir, bindings);
            if expanded == OPAQUE_WRITE || expanded.starts_with('~') {
                out.push(OPAQUE_WRITE.to_string());
                return None;
            }
            let path = Path::new(&expanded);
            cwd = if path.is_absolute() {
                Some(path.to_path_buf())
            } else {
                cwd.as_ref().map(|base| base.join(path))
            };
            index += 1;
        } else if let Some(value) = arg.strip_prefix("CDPATH=") {
            cdpath = !value.is_empty();
            index += 1;
        } else if arg.contains('=') || arg == "-i" || arg == "--ignore-environment" || arg == "--" {
            index += 1;
        } else if arg.starts_with('-') {
            out.push(OPAQUE_WRITE.to_string());
            return None;
        } else {
            if cdpath {
                let nested = argv[index..].join(" ");
                if CWD_SHAPE.is_match(&nested)
                    && (WRITE_SHAPE.is_match(&nested) || FALLBACK_WRITE_SHAPE.is_match(&nested))
                {
                    out.push(OPAQUE_WRITE.to_string());
                    return None;
                }
            }
            return Some((
                argv[index..].to_vec(),
                State {
                    cwd,
                    success: state.success,
                },
            ));
        }
    }
    None
}

fn run_shell(seg: &Segment, args: &[String], state: &State, out: &mut Vec<String>, depth: usize) {
    if let Some(script) = shell_script(args) {
        out.extend(walk(script, state.cwd.as_deref(), depth + 1));
    } else if seg.bodies.is_empty() {
        out.push(OPAQUE_WRITE.to_string());
    }
    for body in &seg.bodies {
        out.extend(walk(body, state.cwd.as_deref(), depth + 1));
    }
}

fn run_body_literals(
    seg: &Segment,
    state: &State,
    bindings: &HashMap<String, String>,
    loops: &HashMap<String, Vec<String>>,
    out: &mut Vec<String>,
) {
    for body in &seg.bodies {
        if INLINE_WRITE_IDIOMS.is_match(body) {
            if body.contains("chdir") {
                out.push(OPAQUE_WRITE.to_string());
                continue;
            }
            let literals = path_literals(body);
            if literals.is_empty() {
                out.push(OPAQUE_WRITE.to_string());
            }
            for literal in literals {
                emit(&literal, state, bindings, loops, out);
            }
        }
    }
}

fn execute(
    seg: &Segment,
    state: &State,
    bindings: &HashMap<String, String>,
    loops: &HashMap<String, Vec<String>>,
    out: &mut Vec<String>,
    depth: usize,
) -> Vec<State> {
    if depth > MAX_DEPTH {
        out.push(OPAQUE_WRITE.to_string());
        return both_statuses(state);
    }
    let tokens = strip_redirects(&seg.argv);
    let start = tokens
        .iter()
        .take_while(|token| shell_assignment(token))
        .count();
    let argv = &tokens[start..];
    let redirects = redirect_targets(&seg.argv);
    for target in &redirects {
        emit(target, state, bindings, loops, out);
    }
    let Some(exe) = argv.first().map(|s| basename(s)) else {
        return both_statuses(state);
    };
    let args = &argv[1..];
    if exe == OPAQUE_WRITE {
        out.push(OPAQUE_WRITE.to_string());
        return both_statuses(state);
    }
    if matches!(exe, "command" | "builtin" | "exec") {
        if exe == "command"
            && args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "-v" | "-V"))
        {
            return both_statuses(state);
        }
        if args.first().is_some_and(|arg| arg.starts_with('-')) {
            out.push(OPAQUE_WRITE.to_string());
            return both_statuses(state);
        }
        return execute(
            &Segment {
                argv: args.to_vec(),
                before: None,
                bodies: seg.bodies.clone(),
            },
            state,
            bindings,
            loops,
            out,
            depth + 1,
        );
    }
    if exe == "cd" {
        return change_dir(argv, state, bindings);
    }
    if matches!(exe, "pushd" | "popd") {
        return vec![
            State {
                cwd: None,
                success: true,
            },
            State {
                cwd: state.cwd.clone(),
                success: false,
            },
        ];
    }
    if matches!(exe, "source" | ".") {
        out.push(OPAQUE_WRITE.to_string());
        return vec![
            State {
                cwd: None,
                success: true,
            },
            State {
                cwd: state.cwd.clone(),
                success: false,
            },
        ];
    }
    execute_program(seg, argv, state, bindings, loops, out, depth)
}

fn execute_program(
    seg: &Segment,
    argv: &[String],
    state: &State,
    bindings: &HashMap<String, String>,
    loops: &HashMap<String, Vec<String>>,
    out: &mut Vec<String>,
    depth: usize,
) -> Vec<State> {
    let exe = basename(&argv[0]);
    let args = &argv[1..];
    if SHELL_RUNNERS.contains(&exe) {
        run_shell(seg, args, state, out, depth);
        return both_statuses(state);
    }
    if exe == "env" {
        if let Some((nested, nested_state)) = env_command(argv, state, bindings, out) {
            execute(
                &Segment {
                    argv: nested,
                    before: None,
                    bodies: seg.bodies.clone(),
                },
                &nested_state,
                bindings,
                loops,
                out,
                depth + 1,
            );
        }
        return both_statuses(state);
    }
    if exe == "eval" {
        let script = args.join(" ");
        if script.contains('$') {
            out.push(OPAQUE_WRITE.to_string());
        } else {
            out.extend(walk(&script, state.cwd.as_deref(), depth + 1));
        }
        if CWD_SHAPE.is_match(&script) {
            return vec![
                State {
                    cwd: None,
                    success: true,
                },
                State {
                    cwd: state.cwd.clone(),
                    success: false,
                },
            ];
        }
        return both_statuses(state);
    }
    if inline_source(&argv[0], args)
        .is_some_and(|source| source.contains("chdir") && INLINE_WRITE_IDIOMS.is_match(&source))
    {
        out.push(OPAQUE_WRITE.to_string());
        return both_statuses(state);
    }
    for target in command_targets(argv) {
        emit(&target, state, bindings, loops, out);
    }
    if exe == "mv" {
        let sources = operands(args, exe);
        for source in sources.iter().take(sources.len().saturating_sub(1)) {
            emit(source, state, bindings, loops, out);
        }
        if args
            .iter()
            .any(|arg| arg == "-t" || arg.starts_with("--target-directory"))
        {
            out.push(OPAQUE_WRITE.to_string());
        }
    }
    if interpreter_basename_matches(exe) {
        run_body_literals(seg, state, bindings, loops, out);
    }
    both_statuses(state)
}

fn walk(command: &str, cwd: Option<&Path>, depth: usize) -> Vec<String> {
    if depth > MAX_DEPTH {
        return vec![OPAQUE_WRITE.to_string()];
    }
    if command.contains("$(") {
        return if FALLBACK_WRITE_SHAPE.is_match(command)
            || WRITE_SHAPE.is_match(command)
            || INLINE_WRITE_IDIOMS.is_match(command)
        {
            vec![OPAQUE_WRITE.to_string()]
        } else {
            Vec::new()
        };
    }
    let (stripped_raw, bodies) = split_heredocs(command);
    let stripped = separate_lines(&stripped_raw);
    let Ok(tokens) = posix_shlex::tokenize(&stripped, posix_shlex::PUNCTUATION_CHARS_FULL) else {
        return vec![OPAQUE_WRITE.to_string()];
    };
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "(" | ")" | "{" | "}"))
    {
        return vec![OPAQUE_WRITE.to_string()];
    }
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "for" | "while" | "until" | "if" | "case"))
    {
        if tokens.iter().any(|t| {
            matches!(
                t.as_str(),
                "cd" | "pushd" | "popd" | "env" | "eval" | "source" | "."
            ) || SHELL_RUNNERS.contains(&basename(t))
        }) {
            return vec![OPAQUE_WRITE.to_string()];
        }
        return write_targets_inner(command, true)
            .iter()
            .flat_map(|t| {
                let state = State {
                    cwd: cwd.map(Path::to_path_buf),
                    success: true,
                };
                let mut found = Vec::new();
                emit(
                    t,
                    &state,
                    &assignments(&stripped),
                    &HashMap::new(),
                    &mut found,
                );
                found
            })
            .collect();
    }
    let bindings = assignments(&stripped);
    let loops = loop_bindings(&stripped, &bindings);
    let list = segments(&tokens, bodies);
    let mut states = vec![State {
        cwd: cwd.map(Path::to_path_buf),
        success: true,
    }];
    let mut out = Vec::new();
    for (index, seg) in list.iter().enumerate() {
        let mut next = Vec::new();
        for state in &states {
            let execute_now = match seg.before.as_deref() {
                Some("&&") => state.success,
                Some("||") => !state.success,
                _ => true,
            };
            if !execute_now {
                next.push(state.clone());
                continue;
            }
            let mut after = execute(seg, state, &bindings, &loops, &mut out, depth);
            if list
                .get(index + 1)
                .and_then(|s| s.before.as_deref())
                .is_some_and(|op| matches!(op, "|" | "&"))
            {
                for result in &mut after {
                    result.cwd = state.cwd.clone();
                }
            }
            next.extend(after);
        }
        states = next
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if states.len() > MAX_STATES {
            return vec![OPAQUE_WRITE.to_string()];
        }
    }
    out
}

pub(super) fn resolve(command: &str, cwd: &Path) -> Vec<String> {
    let mut seen = HashSet::new();
    walk(command, Some(cwd), 0)
        .into_iter()
        .filter(|target| seen.insert(target.clone()))
        .collect()
}
