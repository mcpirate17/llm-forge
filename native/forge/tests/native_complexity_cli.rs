#![cfg(target_os = "linux")]
//! Installed native CLI contracts; no Python, model service, or ambient child environment.
//! Legacy observation mapping:
//! 1 default conductor scan: default_paths_scan_all_four_host_trees_and_default_rank_is_d.
//! 2 worsened, 3 held, 4 improved, 5 new: ratchet_holds_worsens_improves_below_threshold_and_discovers_new_blocks.
//! 6 duplicate minimum: duplicate_baseline_keys_bind_to_lowest_score.
//! 7 below rank: blocks_below_minimum_are_not_ratcheted.
//! 8 fast/full profile: policy_profiles_keep_complexity_in_fast_and_full.
//! 9 host default, 10 configured/fallback: baseline_resolution_flag_environment_forge_pyproject_and_fallback_precedence.
//! 11 relative, 12 absolute: relative_and_absolute_baseline_flags_are_resolved_from_host_root.
//! 13 missing baseline: missing_baseline_is_named_and_report_does_not_require_one.
//! Existing baseline_merge unit tests remain complementary and must not be removed.

#[path = "support/forge_binary.rs"]
mod forge_binary;

use serde_json::Value;
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{symlink, FileTypeExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Case(PathBuf);

impl Case {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-complexity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir(root.join("conductor")).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn command(&self) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .env_clear()
            .current_dir(&self.0)
            .env("PATH", "")
            .env("HOME", &self.0)
            .env("FORGE_LEDGER_DISABLE", "1")
            .env("CONDUCTOR_PYTHON", "/forbidden-python")
            .env("PYO3_PYTHON", "/forbidden-python");
        command
    }

    fn cli(&self, args: &[&str], expected: i32) -> String {
        let mut command = self.command();
        command.arg("complexity").args(args);
        checked(command, expected)
    }

    fn refresh(&self) -> Vec<u8> {
        self.cli(&["refresh-baseline"], 0);
        fs::read(self.0.join("conductor/radon_complexity_baseline.json")).unwrap()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(mut command: Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI exceeded 15s: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn checked(command: Command, expected: i32) -> String {
    let output = run(command);
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    )
}

fn function(name: &str, branches: usize) -> String {
    let mut source = format!("def {name}(x):\n");
    for _ in 0..branches {
        source.push_str("    if x:\n        x = 0\n");
    }
    source.push_str("    return x\n");
    source
}

fn rows(bytes: &[u8]) -> Vec<Value> {
    serde_json::from_slice::<Value>(bytes).unwrap()["findings"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn tool_version_works_without_python_or_a_repository() {
    let case = Case::new();
    fs::remove_dir(case.0.join(".git")).unwrap();
    assert_eq!(
        case.cli(&["--tool-version"], 0).trim(),
        "radon-6.0.1-cpython-3.12"
    );
}

#[test]
fn default_paths_scan_all_four_host_trees_and_default_rank_is_d() {
    let case = Case::new();
    for directory in ["conductor", "research", "aria_core", "aria_designer"] {
        case.write(&format!("{directory}/example.py"), function("widget", 20));
    }
    case.write("elsewhere/ignored.py", function("ignored", 40));
    let output = case.cli(&["report"], 0);
    assert!(output.contains("Total blocks: 4"), "{output}");
    assert!(output.contains("Rank counts: A=0, B=0, C=0, D=4, E=0, F=0"));
    assert!(output.contains("D-F blocks: 4"));
    for directory in ["conductor", "research", "aria_core", "aria_designer"] {
        assert!(output.contains(&format!("21 D {directory}/example.py:1 F widget")));
    }
}

#[test]
fn ratchet_holds_worsens_improves_below_threshold_and_discovers_new_blocks() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let original = case.refresh();
    assert!(case
        .cli(&["check"], 0)
        .contains("Complexity ratchet passed"));
    case.write("conductor/example.py", function("widget", 24));
    let worse = case.cli(&["check"], 1);
    assert!(worse.contains("1 grandfathered blocks worsened"));
    assert!(worse.contains("(21 -> 25)"));
    case.write("conductor/example.py", function("widget", 14));
    let better = case.cli(&["check"], 0);
    assert!(better.contains("1 grandfathered blocks improved; run refresh-baseline:"));
    assert!(better.contains("21 -> 15  conductor/example.py::widget"));
    case.write("conductor/example.py", function("new_widget", 24));
    assert!(case.cli(&["check"], 1).contains("1 new D-F blocks"));
    assert_eq!(
        fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap(),
        original
    );
}

#[test]
fn duplicate_baseline_keys_bind_to_lowest_score() {
    let case = Case::new();
    case.write(
        "conductor/example.py",
        function("widget", 29) + &function("widget", 20),
    );
    let baseline = case.refresh();
    assert_eq!(rows(&baseline).len(), 2);
    case.write("conductor/example.py", function("widget", 24));
    let output = case.cli(&["check"], 1);
    assert!(output.contains("(21 -> 25)"), "{output}");
}

#[test]
fn threshold_boundaries_and_refresh_filter_use_real_scores() {
    let case = Case::new();
    for (name, score) in [
        ("a", 5),
        ("b", 6),
        ("c", 11),
        ("d", 21),
        ("e", 31),
        ("f", 41),
    ] {
        case.write(&format!("conductor/{name}.py"), function(name, score - 1));
    }
    let output = case.cli(&["report", "--min-rank", "A"], 0);
    assert!(output.contains("Rank counts: A=1, B=1, C=1, D=1, E=1, F=1"));
    assert_eq!(rows(&case.refresh()).len(), 3);
    case.cli(&["refresh-baseline", "--min-rank", "F"], 0);
    let bytes = fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap();
    assert_eq!(rows(&bytes)[0]["complexity"], 41);
    assert_eq!(rows(&bytes).len(), 1);
}

#[test]
fn blocks_below_minimum_are_not_ratcheted() {
    let case = Case::new();
    case.refresh();
    case.write("conductor/small.py", function("small", 2));
    assert!(case
        .cli(&["check"], 0)
        .contains("no new or worsened D-F blocks"));
}

#[test]
fn default_excludes_are_replaced_by_supplied_patterns() {
    let case = Case::new();
    for path in [
        "conductor/keep.py",
        "conductor/tests/a.py",
        "conductor/rust/a.py",
        "conductor/tmp/a.py",
        "research/tmp/a.py",
        "conductor/__pycache__/a.py",
    ] {
        case.write(path, function("widget", 20));
    }
    assert!(case.cli(&["report"], 0).contains("Total blocks: 1"));
    let output = case.cli(
        &["report", "--exclude", "*/keep.py", "--exclude", "*/rust/*"],
        0,
    );
    assert!(output.contains("Total blocks: 4"), "{output}");
    assert!(output.contains("conductor/tests/a.py"));
    assert!(!output.contains("conductor/keep.py:"));
}

#[test]
fn repeated_path_flags_and_sort_order_are_stable_by_score_path_and_line() {
    let case = Case::new();
    case.write("conductor/z.py", function("z", 20));
    case.write(
        "conductor/a.py",
        function("first", 20) + &function("second", 20),
    );
    case.write("outside-scan/high.py", function("high", 30));
    let args = [
        "--min-rank",
        "D",
        "report",
        "--path",
        "conductor/z.py",
        "--path",
        "outside-scan",
        "--path",
        "conductor/a.py",
    ];
    let first = case.cli(&args, 0);
    assert_eq!(case.cli(&args, 0), first);
    let positions: Vec<_> = ["F high", "F first", "F second", "F z"]
        .iter()
        .map(|name| first.find(name).unwrap())
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{first}"
    );
}

#[test]
fn utf8_class_methods_keep_class_qualified_baseline_keys() {
    let case = Case::new();
    let method = function("méthode", 20)
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>();
    case.write("conductor/café.py", format!("class Café:\n{method}"));
    let baseline = case.refresh();
    assert!(baseline.is_ascii());
    let values = rows(&baseline);
    let method = values
        .iter()
        .find(|row| row["key"] == "conductor/café.py::Café.méthode")
        .unwrap();
    assert_eq!(method["type"], "M");
    assert_eq!(method["name"], "méthode");
    assert_eq!(method["line"], 2);
    assert_eq!(method["complexity"], 21);
    case.cli(&["check"], 0);
}

#[test]
fn report_truncates_details_after_fifty_without_truncating_counts_or_baseline() {
    let case = Case::new();
    for index in 0..55 {
        case.write(&format!("conductor/f{index:02}.py"), function("widget", 20));
    }
    let output = case.cli(&["report"], 0);
    assert!(output.contains("Total blocks: 55"));
    assert!(output.contains("D-F blocks: 55"));
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains(" F widget"))
            .count(),
        50
    );
    assert_eq!(rows(&case.refresh()).len(), 55);
}

#[test]
fn missing_baseline_is_named_and_report_does_not_require_one() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    case.cli(&["report", "--baseline", "missing.json"], 0);
    let output = case.cli(&["check", "--baseline", "missing.json"], 1);
    assert!(output.contains("missing.json"), "{output}");
    assert!(!case.0.join("missing.json").exists());
}

#[test]
fn malformed_baseline_fails_after_source_scan_and_is_never_rewritten() {
    let case = Case::new();
    case.cli(&["refresh-baseline", "--baseline", "broken.json"], 0);
    let path = case.write("broken.json", b"{broken");
    case.cli(&["check", "--baseline", "broken.json"], 1);
    case.write("conductor/broken.py", "def broken(:\n");
    let output = case.cli(&["check", "--baseline", "broken.json"], 2);
    assert!(output.contains("conductor/broken.py"), "{output}");
    assert_eq!(fs::read(path).unwrap(), b"{broken");
}

#[test]
fn malformed_row_types_are_refused_without_coercion_or_baseline_modification() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let refreshed: Value = serde_json::from_slice(&case.refresh()).unwrap();
    let baseline = case.0.join("conductor/radon_complexity_baseline.json");
    // Intentional admission correction: native code refuses malformed row types
    // instead of inheriting Python's str()/int() coercion of keys and scores.
    for (field, malformed) in [
        ("complexity", serde_json::json!("21")),
        ("complexity", serde_json::json!(true)),
        ("complexity", serde_json::json!(21.5)),
        ("key", serde_json::json!(42)),
    ] {
        let mut corrupted = refreshed.clone();
        corrupted["findings"][0][field] = malformed;
        let bytes = serde_json::to_vec(&corrupted).unwrap();
        fs::write(&baseline, &bytes).unwrap();
        case.cli(&["check"], 1);
        assert_eq!(fs::read(&baseline).unwrap(), bytes);
    }
}

#[test]
fn baseline_metadata_does_not_change_source_selection_threshold_or_ratchet() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let mut refreshed: Value = serde_json::from_slice(&case.refresh()).unwrap();
    refreshed["minimum_rank"] = serde_json::json!("F");
    refreshed["paths"] = serde_json::json!(["does-not-exist"]);
    refreshed["excludes"] = serde_json::json!(["*"]);
    refreshed["extra_metadata"] = serde_json::json!({"ignored": true});
    let bytes = serde_json::to_vec(&refreshed).unwrap();
    let baseline = case.0.join("conductor/radon_complexity_baseline.json");
    fs::write(&baseline, &bytes).unwrap();
    assert!(case
        .cli(&["check"], 0)
        .contains("no new or worsened D-F blocks"));
    // A real source regression still fails despite metadata that would hide it
    // if any stored scanning options were applied to this invocation.
    case.write("conductor/example.py", function("widget", 24));
    let output = case.cli(&["check"], 1);
    assert!(output.contains("(21 -> 25)"), "{output}");
    assert_eq!(fs::read(baseline).unwrap(), bytes);
}

#[test]
fn syntax_and_utf8_errors_precede_ratchet_and_refresh_never_writes() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let original = case.refresh();
    for invalid in [
        b"def broken(:\n".as_slice(),
        b"\xff\xfe".as_slice(),
        b"\xef\xbb\xbfdef bom(): pass\n".as_slice(),
    ] {
        case.write("conductor/broken.py", invalid);
        for command in ["report", "check", "refresh-baseline"] {
            let output = case.cli(&[command], 2);
            assert!(output.contains("conductor/broken.py"), "{output}");
            assert_eq!(
                fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap(),
                original
            );
        }
        case.cli(&["refresh-baseline", "--baseline", "new.json"], 2);
        assert!(!case.0.join("new.json").exists());
    }
}

#[test]
fn relative_and_absolute_baseline_flags_are_resolved_from_host_root() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    fs::create_dir(case.0.join("ratchet")).unwrap();
    fs::create_dir(case.0.join("nested")).unwrap();
    let mut command = case.command();
    command.current_dir(case.0.join("nested")).args([
        "complexity",
        "refresh-baseline",
        "--baseline",
        "ratchet/base.json",
    ]);
    checked(command, 0);
    assert!(case.0.join("ratchet/base.json").is_file());
    assert!(!case.0.join("nested/ratchet").exists());
    let outside = Case::new();
    let target = outside.0.join("absolute.json");
    let mut command = case.command();
    command
        .args(["complexity", "refresh-baseline", "--baseline"])
        .arg(&target);
    checked(command, 0);
    assert_eq!(rows(&fs::read(target).unwrap())[0]["complexity"], 21);
}

#[test]
fn baseline_resolution_flag_environment_forge_pyproject_and_fallback_precedence() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nradon_complexity_baseline='pyproject.json'\n",
    );
    case.write(
        "forge.toml",
        "schema_version=1\n[conductor]\nradon_complexity_baseline='forge.json'\n",
    );
    let mut command = case.command();
    command.env("CONDUCTOR_RADON_BASELINE", "env.json").args([
        "complexity",
        "refresh-baseline",
        "--baseline",
        "flag.json",
    ]);
    checked(command, 0);
    assert!(case.0.join("flag.json").is_file());
    assert!(!case.0.join("env.json").exists());
    let mut command = case.command();
    command
        .env("CONDUCTOR_RADON_BASELINE", "env.json")
        .args(["complexity", "refresh-baseline"]);
    checked(command, 0);
    assert!(case.0.join("env.json").is_file());
    case.cli(&["refresh-baseline"], 0);
    assert!(case.0.join("forge.json").is_file());
    fs::remove_file(case.0.join("forge.toml")).unwrap();
    case.cli(&["refresh-baseline"], 0);
    assert!(case.0.join("pyproject.json").is_file());
    fs::remove_file(case.0.join("pyproject.toml")).unwrap();
    case.refresh();
}

#[test]
fn forge_config_is_authoritative_even_without_a_baseline_setting() {
    let case = Case::new();
    case.write("forge.toml", "schema_version=1\n[conductor]\n");
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nradon_complexity_baseline='wrong.json'\n",
    );
    case.refresh();
    assert!(!case.0.join("wrong.json").exists());
}

#[test]
fn host_environment_and_explicit_repo_select_host_sources_and_baseline() {
    let ambient = Case::new();
    let host = Case::new();
    host.write("conductor/example.py", function("widget", 20));
    let mut command = ambient.command();
    command
        .env("CONDUCTOR_HOST_ROOT", &host.0)
        .args(["complexity", "refresh-baseline"]);
    checked(command, 0);
    assert!(host
        .0
        .join("conductor/radon_complexity_baseline.json")
        .exists());
    assert!(!ambient
        .0
        .join("conductor/radon_complexity_baseline.json")
        .exists());
    for before in [true, false] {
        let mut command = ambient.command();
        if before {
            command
                .args(["complexity", "--repo"])
                .arg(&host.0)
                .arg("check");
        } else {
            command.args(["complexity", "check", "--repo"]).arg(&host.0);
        }
        assert!(checked(command, 0).contains("Complexity ratchet passed"));
    }
}

#[test]
fn invalid_cli_arguments_never_refresh_a_baseline() {
    let case = Case::new();
    for args in [
        vec!["refresh-baseline", "--min-rank", "G"],
        vec!["refresh-baseline", "--unknown"],
        vec!["unknown"],
        vec!["refresh-baseline", "--repo", "relative"],
    ] {
        case.cli(&args, 2);
        assert!(!case
            .0
            .join("conductor/radon_complexity_baseline.json")
            .exists());
    }
}

#[test]
fn policy_profiles_keep_complexity_in_fast_and_full() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy: toml::Value = fs::read_to_string(root.join("candidate_policy.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let profiles = policy["checks"]["complexity"]["profiles"]
        .as_array()
        .unwrap();
    assert!(profiles.iter().any(|value| value.as_str() == Some("fast")));
    assert!(profiles.iter().any(|value| value.as_str() == Some("full")));
}

#[test]
fn fifo_source_is_refused_without_refresh_write() {
    let case = Case::new();
    let fifo = case.0.join("conductor/fifo.py");
    let fifo_c = CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    case.cli(&["refresh-baseline", "--path", "conductor/fifo.py"], 2);
    assert!(!case
        .0
        .join("conductor/radon_complexity_baseline.json")
        .exists());
    fs::remove_file(fifo).unwrap();
}

#[test]
fn fifo_baseline_refresh_is_refused_without_blocking_or_replacing_fifo() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let fifo = case.0.join("baseline.fifo");
    let fifo_c = CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    // Baseline I/O retains the legacy exit 1, with safe refusal replacing a
    // potentially blocking write; source FIFO admission separately returns 2.
    case.cli(&["refresh-baseline", "--baseline", "baseline.fifo"], 1);
    assert!(fs::symlink_metadata(fifo).unwrap().file_type().is_fifo());
}

#[test]
fn hidden_python_names_including_bare_dot_py_are_scanned() {
    let case = Case::new();
    case.write("conductor/.py", function("bare", 20));
    case.write("conductor/.hidden.py", function("hidden", 20));
    let output = case.cli(&["report"], 0);
    assert!(output.contains("Total blocks: 2"), "{output}");
    assert!(output.contains("conductor/.py:1 F bare"));
    assert!(output.contains("conductor/.hidden.py:1 F hidden"));
}

#[test]
fn missing_scopes_are_empty_and_absolute_outside_sources_are_supported() {
    let host = Case::new();
    let outside = Case::new();
    let source = outside.write("external.py", function("widget", 20));
    assert!(host
        .cli(&["report", "--path", "does-not-exist"], 0)
        .contains("Total blocks: 0"));
    let mut command = host.command();
    command
        .args([
            "complexity",
            "refresh-baseline",
            "--exclude",
            "never-matches",
            "--path",
        ])
        .arg(&source);
    checked(command, 0);
    let bytes = fs::read(host.0.join("conductor/radon_complexity_baseline.json")).unwrap();
    assert_eq!(rows(&bytes)[0]["path"], source.to_str().unwrap());
}

#[test]
fn explicit_symlink_scope_resolves_to_target_and_nested_directory_links_are_not_followed() {
    let case = Case::new();
    let outside = Case::new();
    outside.write("nested/example.py", function("widget", 20));
    symlink(outside.0.join("nested"), case.0.join("conductor/link")).unwrap();
    assert!(case.cli(&["report"], 0).contains("Total blocks: 0"));
    // An explicit outside /tmp path still obeys the default exclusion policy.
    assert!(case
        .cli(&["report", "--path", "conductor/link"], 0)
        .contains("Total blocks: 0"));
    let output = case.cli(
        &[
            "report",
            "--path",
            "conductor/link",
            "--exclude",
            "never-matches",
        ],
        0,
    );
    assert!(output.contains("Total blocks: 1"), "{output}");
    assert!(output.contains(outside.0.join("nested/example.py").to_str().unwrap()));
}

#[test]
fn source_size_bound_is_inclusive_and_oversize_refresh_preserves_baseline() {
    let case = Case::new();
    let original = case.refresh();
    let mut source = b"#".to_vec();
    source.resize(1024 * 1024, b' ');
    case.write("conductor/large.py", &source);
    case.cli(&["report"], 0);
    source.push(b' ');
    case.write("conductor/large.py", source);
    case.cli(&["refresh-baseline"], 2);
    assert_eq!(
        fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap(),
        original
    );
}

#[test]
fn repeated_scope_occurrences_are_preserved_and_count_toward_admission_bound() {
    let case = Case::new();
    case.write("conductor/example.py", function("widget", 20));
    let output = case.cli(
        &[
            "report",
            "--path",
            "conductor/example.py",
            "--path",
            "conductor/example.py",
        ],
        0,
    );
    assert!(output.contains("Total blocks: 2"), "{output}");
    let mut command = case.command();
    command.args(["complexity", "refresh-baseline"]);
    for _ in 0..4097 {
        command.args(["--path", "conductor/example.py"]);
    }
    checked(command, 2);
    assert!(!case
        .0
        .join("conductor/radon_complexity_baseline.json")
        .exists());
}

#[test]
fn aggregate_source_bytes_count_duplicate_scope_work_and_prevent_refresh_write() {
    let case = Case::new();
    let original = case.refresh();
    let mut source = b"#".to_vec();
    source.resize(1024 * 1024, b' ');
    case.write("conductor/large.py", source);
    let mut command = case.command();
    command.args(["complexity", "refresh-baseline"]);
    for _ in 0..33 {
        command.args(["--path", "conductor/large.py"]);
    }
    checked(command, 2);
    assert_eq!(
        fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap(),
        original
    );
}

#[test]
fn repeated_scope_flags_on_both_sides_of_command_keep_one_ordered_inventory() {
    let case = Case::new();
    case.write("conductor/a.py", function("alpha", 20));
    case.write("conductor/b.py", function("beta", 20));
    case.cli(
        &[
            "--path",
            "conductor/b.py",
            "refresh-baseline",
            "--path",
            "conductor/a.py",
        ],
        0,
    );
    let raw = fs::read(case.0.join("conductor/radon_complexity_baseline.json")).unwrap();
    let value: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        value["paths"],
        serde_json::json!(["conductor/b.py", "conductor/a.py"])
    );
    let rows = rows(&raw);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "alpha");
    assert_eq!(rows[1]["name"], "beta");
}
