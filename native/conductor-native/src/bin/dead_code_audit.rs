//! Run Vulture and adjudicate its candidates against repository reference evidence.
use anyhow::{bail, Context, Result};
use conductor_native::dead_code::{adjudicate, parse_vulture_output, AuditReport};
use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

const DEFAULT_OUTPUT: &str = ".forge/reports/dead-code/latest.json";
const DEFAULT_EXCLUDE: &str = "*/tests/*,*/catalog/*,*/__pycache__/*";
const HELP: &str = "dead-code-audit: evidence for Vulture candidates (not a deletion plan)

Usage: dead-code-audit [OPTIONS]
  --root PATH               Repository root (default: current directory)
  --path PATH               Vulture source path relative to root; repeatable (default: .)
  --whitelist PATH          Vulture whitelist relative to root; repeatable
  --vulture PATH            Executable (default: ROOT/.venv/bin/vulture)
  --min-confidence N        Vulture confidence, 0..100 (default: 60)
  --exclude GLOBS           Vulture exclusions (default: */tests/*,*/catalog/*,*/__pycache__/*)
  --vulture-output PATH     Replay captured Vulture stdout instead of running Vulture
  --exclude-reference PATH  Exclude a reference file/directory relative to root; repeatable
  --output PATH             JSON report relative to root
                            (default: .forge/reports/dead-code/latest.json)
  -h, --help                Show this help

References are scanned across the whole repository. Output, replay input and whitelists
are excluded from the reference index. Existing reports in .forge/reports/dead-code
are excluded too. Exit 0: complete report; exit 2: incomplete scan or execution error.
";

#[derive(Debug)]
struct Options {
    root: PathBuf,
    paths: Vec<PathBuf>,
    whitelists: Vec<PathBuf>,
    vulture: Option<PathBuf>,
    min_confidence: u8,
    exclude: OsString,
    replay: Option<PathBuf>,
    excluded_references: Vec<PathBuf>,
    output: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            paths: Vec::new(),
            whitelists: Vec::new(),
            vulture: None,
            min_confidence: 60,
            exclude: DEFAULT_EXCLUDE.into(),
            replay: None,
            excluded_references: Vec::new(),
            output: PathBuf::from(DEFAULT_OUTPUT),
        }
    }
}

fn parse_options() -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            print!("{HELP}");
            return Ok(None);
        }
        let value = args
            .next()
            .with_context(|| format!("missing value for {}", arg.to_string_lossy()))?;
        match arg.to_str() {
            Some("--root") => options.root = value.into(),
            Some("--path") => options.paths.push(value.into()),
            Some("--whitelist") => options.whitelists.push(value.into()),
            Some("--vulture") => options.vulture = Some(value.into()),
            Some("--exclude") => options.exclude = value,
            Some("--vulture-output") => options.replay = Some(value.into()),
            Some("--exclude-reference") => options.excluded_references.push(value.into()),
            Some("--output") => options.output = value.into(),
            Some("--min-confidence") => {
                options.min_confidence = value
                    .to_str()
                    .context("confidence is not UTF-8")?
                    .parse()
                    .context("confidence must be an integer from 0 to 100")?;
                if options.min_confidence > 100 {
                    bail!("confidence must be an integer from 0 to 100");
                }
            }
            _ => bail!("unknown option: {}", arg.to_string_lossy()),
        }
    }
    if options.paths.is_empty() {
        options.paths.push(PathBuf::from("."));
    }
    options.root = options
        .root
        .canonicalize()
        .context("invalid repository root")?;
    if !options.root.is_dir() {
        bail!("repository root is not a directory");
    }
    Ok(Some(options))
}

#[derive(Serialize)]
struct Invocation {
    root: PathBuf,
    source_paths: Vec<PathBuf>,
    whitelists: Vec<PathBuf>,
    command: Option<Vec<String>>,
    vulture_exit_code: Option<i32>,
    replay_input: Option<PathBuf>,
    excluded_references: Vec<PathBuf>,
    elapsed_ms: u128,
}

#[derive(Serialize)]
struct Report {
    #[serde(flatten)]
    audit: AuditReport,
    invocation: Invocation,
}

fn rooted(root: &Path, path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in root.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

fn validate_output(options: &Options, output: &Path) -> Result<()> {
    if output.extension().and_then(|value| value.to_str()) != Some("json") {
        bail!("--output must name a .json report, not a source file");
    }
    let resolved = if output.exists() {
        output.canonicalize()?
    } else {
        output.to_path_buf()
    };
    for input in options
        .replay
        .iter()
        .chain(&options.whitelists)
        .chain(&options.paths)
    {
        let path = rooted(&options.root, input);
        let path = if path.exists() {
            path.canonicalize()?
        } else {
            path
        };
        if resolved == path {
            bail!(
                "report output collides with audit input: {}",
                input.display()
            );
        }
    }
    Ok(())
}

fn vulture_output(options: &Options, invocation: &mut Invocation) -> Result<String> {
    if let Some(path) = &options.replay {
        return fs::read_to_string(rooted(&options.root, path))
            .context("reading captured Vulture stdout");
    }
    let executable = options.vulture.as_ref().map_or_else(
        || options.root.join(".venv/bin/vulture"),
        |path| rooted(&options.root, path),
    );
    let mut command = Command::new(&executable);
    command.current_dir(&options.root);
    command.args(&options.paths).args(&options.whitelists);
    command.args(["--min-confidence", &options.min_confidence.to_string()]);
    command
        .arg("--sort-by-size")
        .arg("--exclude")
        .arg(&options.exclude);
    invocation.command = Some(
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
    );
    let output = command.output().with_context(|| {
        format!(
            "running Vulture at {}; use --vulture to select it",
            executable.display()
        )
    })?;
    invocation.vulture_exit_code = output.status.code();
    if !matches!(output.status.code(), Some(0 | 3)) || !output.stderr.is_empty() {
        bail!(
            "Vulture failed or reported diagnostics (status {}):\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    String::from_utf8(output.stdout).context("Vulture stdout is not UTF-8")
}

fn write_report(path: &Path, report: &Report) -> Result<()> {
    let parent = path.parent().context("output needs a parent directory")?;
    fs::create_dir_all(parent).context("creating report directory")?;
    let name = path
        .file_name()
        .context("output needs a filename")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, report)?;
        writeln!(writer)?;
        writer.flush()?;
        fs::rename(&temporary, path).context("publishing report")
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            eprintln!(
                "could not remove incomplete report {}: {error}",
                temporary.display()
            );
        }
    }
    result
}

fn run(options: Options) -> Result<bool> {
    let start = Instant::now();
    let output = rooted(&options.root, &options.output);
    validate_output(&options, &output)?;
    let mut excluded = options.excluded_references.clone();
    excluded.extend(options.whitelists.iter().cloned());
    excluded.extend(options.replay.iter().cloned());
    excluded.push(output.clone());
    excluded.push(options.root.join(".forge/reports/dead-code"));
    let excluded: Vec<PathBuf> = excluded
        .iter()
        .map(|path| rooted(&options.root, path))
        .collect();
    let mut invocation = Invocation {
        root: options.root.clone(),
        source_paths: options.paths.clone(),
        whitelists: options.whitelists.clone(),
        command: None,
        vulture_exit_code: None,
        replay_input: options
            .replay
            .as_ref()
            .map(|path| rooted(&options.root, path)),
        excluded_references: excluded.clone(),
        elapsed_ms: 0,
    };
    let findings = parse_vulture_output(&vulture_output(&options, &mut invocation)?)?;
    let audit = adjudicate(&options.root, findings, &excluded)?;
    invocation.elapsed_ms = start.elapsed().as_millis();
    let report = Report { audit, invocation };
    write_report(&output, &report)?;
    println!(
        "{} total {}",
        serde_json::to_string(&report.audit.counts)?,
        report.audit.findings.len()
    );
    println!("{} ({} ms)", output.display(), report.invocation.elapsed_ms);
    if !report.audit.complete {
        eprintln!(
            "INCOMPLETE: {} scan diagnostics; inspect the report before using verdicts",
            report.audit.diagnostics.len()
        );
    }
    Ok(report.audit.complete)
}

fn main() -> ExitCode {
    match parse_options().and_then(|options| options.map_or(Ok(true), run)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("dead-code-audit: {error:#}");
            ExitCode::from(2)
        }
    }
}
