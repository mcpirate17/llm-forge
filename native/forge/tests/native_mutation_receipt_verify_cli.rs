#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
use git2::{ObjectType, Oid, Repository, RepositoryInitOptions, Signature};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
const MANIFEST: &str =
    "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
const SOURCE: &str = "src/conductor/mutation_attribution.py";
const RECEIPT: &[u8] =
    include_bytes!("../../conductor-native/tests/fixtures/mutation_verifier/original-receipt.json");
struct Fixture {
    root: PathBuf,
    repo: Repository,
    receipt: PathBuf,
    tree: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(forge::resource_id("mutation-receipt-cli").unwrap());
        fs::create_dir(&root).unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repo = Repository::init_opts(root.join("repo"), &options).unwrap();
        let receipt = root.join("receipt.json");
        fs::write(&receipt, RECEIPT).unwrap();
        let mut this = Self {
            root,
            repo,
            receipt,
            tree: String::new(),
        };
        this.stage(
            MANIFEST,
            include_bytes!("../../conductor-native/tests/fixtures/mutation_verifier/manifest.json"),
        );
        this.stage(
            SOURCE,
            include_bytes!("../../conductor-native/tests/fixtures/mutation_verifier/source.py.txt"),
        );
        this.stage(
            "src/conductor/test_mutation_attribution.py",
            include_bytes!("../../conductor-native/tests/fixtures/mutation_verifier/test.py.txt"),
        );
        this.commit();
        this.tree = this
            .repo
            .head()
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .id()
            .to_string();
        this
    }
    fn stage(&self, path: &str, bytes: &[u8]) {
        let file = self.root.join("repo").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn remove(&self, path: &str) {
        let mut index = self.repo.index().unwrap();
        index.remove_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn tree(&self) -> String {
        self.repo.index().unwrap().write_tree().unwrap().to_string()
    }
    fn commit(&self) {
        let tree = self
            .repo
            .find_tree(self.repo.index().unwrap().write_tree().unwrap())
            .unwrap();
        let sig = Signature::now("automatic-corpus-fixture", "fixture@example.invalid").unwrap();
        let parent = self.repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        self.repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "automatic corpus bytes",
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap();
    }
    fn command(&self, tree: &str) -> Command {
        let mut cmd = Command::new(forge_binary::path());
        cmd.args(["receipt", "verify", "--receipt"])
            .arg(&self.receipt)
            .args(["--tree", tree]);
        cmd.env_clear()
            .current_dir(self.root.join("repo"))
            .env("PATH", "")
            .env("HOME", &self.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
        cmd
    }
    fn run(&self, tree: &str) -> Output {
        probe(&mut self.command(tree))
    }
    fn json(&self, tree: &str, code: i32) -> Value {
        let out = probe(self.command(tree).arg("--json"));
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn corrupt_receipt(&self, edit: impl FnOnce(&mut Value)) {
        // Negative corruption of existing automatic bytes; no PASS campaign artifact is invented.
        let mut value: Value = serde_json::from_slice(RECEIPT).unwrap();
        edit(&mut value);
        fs::write(&self.receipt, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn probe(command: &mut Command) -> Output {
    forge::process_probe::run(command, Duration::from_secs(10), 1024 * 1024).unwrap()
}
fn status<'a>(v: &'a Value, part: &str) -> &'a Value {
    &v["checks"][part]["status"]
}
#[test]
fn original_automatic_bytes_bind_all_four_parts_without_certifying_campaign_pass() {
    let f = Fixture::new();
    let out = f.run("HEAD");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let human = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<_> = human.lines().collect();
    assert!(lines[0].starts_with("repo_root="));
    assert_eq!(lines[1], format!("tree_oid={}", f.tree));
    assert!(lines[2].starts_with("PASS "));
    let verdict = f.json(&f.tree, 0);
    assert_eq!(verdict["tree_oid"], f.tree);
    for part in [
        "manifest_blob_in_tree",
        "manifest_sha256",
        "source_pins",
        "inventory_digest",
    ] {
        assert_eq!(status(&verdict, part), "PASS");
    }
    let original: Value = serde_json::from_slice(RECEIPT).unwrap();
    assert!(original["outcome_counts"]["NO_COVERAGE"].as_u64().unwrap() > 0);
    // PASS here names target-tree byte bindings only. The strict campaign verifier rejects NO_COVERAGE.
}
#[test]
fn json_resolution_uses_stderr_and_default_repo_discovers_from_subdirectory() {
    let f = Fixture::new();
    let out = probe(
        f.command("HEAD")
            .arg("--json")
            .current_dir(f.root.join("repo/src")),
    );
    assert_eq!(out.status.code(), Some(0));
    let verdict: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        verdict["repo_root"],
        f.root
            .join("repo")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("tree_oid={}", f.tree)));
    let explicit = probe(
        f.command("HEAD")
            .arg("--repo")
            .arg(f.root.join("repo"))
            .current_dir(&f.root),
    );
    assert_eq!(explicit.status.code(), Some(0));
}
#[test]
fn dirty_host_and_index_bytes_do_not_change_named_tree_measurement() {
    let f = Fixture::new();
    f.stage(SOURCE, b"dirty staged source\n");
    fs::write(
        f.root.join("repo").join(MANIFEST),
        b"dirty invalid manifest",
    )
    .unwrap();
    assert_eq!(f.json("HEAD", 0)["status"], "PASS");
    let altered = f.json(&f.tree(), 1);
    assert_eq!(status(&altered, "source_pins"), "FAIL");
    assert_eq!(status(&altered, "inventory_digest"), "FAIL");
}
#[test]
fn missing_manifest_blocks_dependent_checks_and_returns_measured_failure() {
    let f = Fixture::new();
    f.remove(MANIFEST);
    let v = f.json(&f.tree(), 1);
    assert_eq!(status(&v, "manifest_blob_in_tree"), "FAIL");
    for part in ["manifest_sha256", "source_pins", "inventory_digest"] {
        assert_eq!(status(&v, part), "BLOCKED");
    }
}
#[test]
fn manifest_hash_drift_and_receipt_inventory_lie_are_distinct_failures() {
    let f = Fixture::new();
    let mut manifest =
        include_bytes!("../../conductor-native/tests/fixtures/mutation_verifier/manifest.json")
            .to_vec();
    manifest.push(b' ');
    f.stage(MANIFEST, &manifest);
    let v = f.json(&f.tree(), 1);
    assert_eq!(status(&v, "manifest_sha256"), "FAIL");
    assert_eq!(status(&v, "source_pins"), "PASS");
    assert_eq!(status(&v, "inventory_digest"), "PASS");
    let f = Fixture::new();
    f.corrupt_receipt(|v| v["source_sha256"][SOURCE] = json!("0".repeat(64)));
    let v = f.json("HEAD", 1);
    for part in ["manifest_blob_in_tree", "manifest_sha256", "source_pins"] {
        assert_eq!(status(&v, part), "PASS");
    }
    assert_eq!(status(&v, "inventory_digest"), "FAIL");
    assert_eq!(
        v["checks"]["inventory_digest"]["divergence"]["hash_mismatch"],
        json!([SOURCE])
    );
}
#[test]
fn missing_source_blocks_inventory_and_names_the_pin() {
    let f = Fixture::new();
    f.remove(SOURCE);
    let v = f.json(&f.tree(), 1);
    assert_eq!(status(&v, "source_pins"), "FAIL");
    assert_eq!(status(&v, "inventory_digest"), "BLOCKED");
    assert!(v["failures"].to_string().contains(SOURCE));
}
#[test]
fn malformed_receipt_manifest_and_unusable_context_are_refused_with_exit_four() {
    let f = Fixture::new();
    fs::write(&f.receipt, b"").unwrap();
    let out = f.run("HEAD");
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("receipt file is empty"));
    assert_eq!(f.json("HEAD", 4)["status"], "REFUSED");
    fs::write(&f.receipt, RECEIPT).unwrap();
    assert_eq!(f.json("deadbeef", 4)["status"], "REFUSED");
    let overlong = f.json(&"r".repeat(4097), 4);
    assert!(overlong["error"]
        .as_str()
        .unwrap()
        .contains("invalid --tree reference"));
    let out = probe(f.command("HEAD").arg("--repo").arg(f.root.join("missing")));
    assert_eq!(out.status.code(), Some(4));
    f.stage(MANIFEST, b"{not JSON");
    assert_eq!(f.json(&f.tree(), 4)["status"], "REFUSED");
}
#[test]
fn duplicate_json_keys_and_input_limits_refuse_without_silent_pin_omission() {
    let f = Fixture::new();
    fs::write(&f.receipt, b"{\"manifest\":\"one\",\"manifest\":\"two\"}").unwrap();
    assert_eq!(f.json("HEAD", 4)["status"], "REFUSED");
    fs::write(&f.receipt, RECEIPT).unwrap();
    f.stage(MANIFEST, &vec![b' '; 2 * 1024 * 1024 + 1]);
    assert_eq!(f.json(&f.tree(), 4)["status"], "REFUSED");
    let f = Fixture::new();
    f.corrupt_receipt(|v| {
        let pins = v["source_sha256"].as_object_mut().unwrap();
        for i in 0..4097 {
            pins.insert(format!("extra/{i}"), json!("0".repeat(64)));
        }
    });
    assert_eq!(f.json("HEAD", 4)["status"], "REFUSED");
}
#[test]
fn target_tree_symlink_mode_blobs_are_hashed_without_following_host_links() {
    let f = Fixture::new();
    let mut index = f.repo.index().unwrap();
    let mut entry = index.get_path(Path::new(SOURCE), 0).unwrap();
    entry.mode = 0o120000;
    index.add(&entry).unwrap();
    index.write().unwrap();
    assert_eq!(f.json(&f.tree(), 0)["status"], "PASS");
}
#[test]
fn pasted_smart_dashes_keep_the_old_leaf_cli_contract() {
    let f = Fixture::new();
    let mut cmd = Command::new(forge_binary::path());
    cmd.args(["receipt", "verify", "—receipt"])
        .arg(&f.receipt)
        .args(["–tree", "HEAD", "—repo"])
        .arg(f.root.join("repo"))
        .arg("—json")
        .env("PATH", "");
    let out = probe(&mut cmd);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["tree_oid"],
        f.tree
    );
}

#[test]
fn annotated_tags_peel_to_the_same_target_tree() {
    let f = Fixture::new();
    let commit = f.repo.head().unwrap().peel_to_commit().unwrap();
    let sig = Signature::now("tag-fixture", "fixture@example.invalid").unwrap();
    f.repo
        .tag(
            "binding-target",
            commit.as_object(),
            &sig,
            "original automatic corpus tree",
            false,
        )
        .unwrap();
    assert_eq!(f.json("binding-target", 0)["tree_oid"], f.tree);
}
#[test]
fn ordinary_candidate_receipts_are_not_the_mutation_binding_surface() {
    let f = Fixture::new();
    let candidate = forge::candidate_receipt::seal(
        &serde_json::to_vec(&json!({"decision":"fail","candidate":{"tree_oid":f.tree}})).unwrap(),
    )
    .unwrap();
    fs::write(&f.receipt, serde_json::to_vec(candidate.payload()).unwrap()).unwrap();
    assert_eq!(f.json("HEAD", 4)["status"], "REFUSED");
}

fn binding_revisions(f: &Fixture) -> Vec<String> {
    // Two real commits have identical original automatic corpus tree bytes.
    // HEAD~1 therefore exercises ordinary libgit2 ancestry without changing bindings.
    f.commit();
    let commit = f.repo.head().unwrap().peel_to_commit().unwrap();
    f.repo.branch("binding-branch", &commit, false).unwrap();
    f.repo
        .tag_lightweight("binding-light", commit.as_object(), false)
        .unwrap();
    let sig = Signature::now("tag-fixture", "fixture@example.invalid").unwrap();
    let inner = f
        .repo
        .tag("binding-inner", commit.as_object(), &sig, "inner", false)
        .unwrap();
    let inner = f.repo.find_object(inner, Some(ObjectType::Tag)).unwrap();
    let outer = f
        .repo
        .tag("binding-outer", &inner, &sig, "outer", false)
        .unwrap();
    vec![
        commit.id().to_string(),
        f.tree.clone(),
        "HEAD".into(),
        "main".into(),
        "refs/heads/main".into(),
        "binding-branch".into(),
        "refs/heads/binding-branch".into(),
        "binding-light".into(),
        "refs/tags/binding-light".into(),
        "binding-outer".into(),
        "refs/tags/binding-outer".into(),
        outer.to_string(),
        "HEAD^{tree}".into(),
        "HEAD^0".into(),
        "HEAD~1".into(),
    ]
}

fn assert_binding_revisions(f: &Fixture, revisions: &[String]) {
    let expected = f.json(&f.tree, 0);
    let root = f.root.join("repo").canonicalize().unwrap();
    for revision in revisions {
        let actual = f.json(revision, 0);
        assert_eq!(actual["status"], "PASS", "revision={revision}");
        assert_eq!(actual["tree_oid"], f.tree, "revision={revision}");
        assert_eq!(
            actual["repo_root"],
            root.to_str().unwrap(),
            "revision={revision}"
        );
        assert_eq!(actual["checks"], expected["checks"], "revision={revision}");
    }
}

fn pack_fixture_objects_and_remove_only_their_loose_copies(f: &Fixture) {
    let odb = f.repo.odb().unwrap();
    let mut ids = Vec::new();
    odb.foreach(|oid| {
        ids.push(*oid);
        true
    })
    .unwrap();
    let directory = f.repo.path().join("objects/pack");
    fs::create_dir_all(&directory).unwrap();
    let mut builder = f.repo.packbuilder().unwrap();
    builder.set_threads(1);
    for id in &ids {
        builder.insert_object(*id, None).unwrap();
    }
    builder.write(&directory, 0o600).unwrap();
    drop(builder);
    assert!(fs::read_dir(&directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|value| value == "pack")
    }));
    for id in ids {
        let hex = id.to_string();
        let loose = f
            .repo
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        if loose.exists() {
            fs::remove_file(&loose).unwrap();
        }
        assert!(!loose.exists());
    }
}

#[test]
fn full_git_revisions_bind_identically_with_loose_and_packed_object_storage() {
    let f = Fixture::new();
    let revisions = binding_revisions(&f);
    assert_binding_revisions(&f, &revisions);
    // Use actual native pack/index generation, then force fresh CLI processes
    // to resolve every revision with no loose copies of fixture objects left.
    pack_fixture_objects_and_remove_only_their_loose_copies(&f);
    f.repo.refdb().unwrap().compress().unwrap();
    assert!(f.repo.path().join("packed-refs").is_file());
    assert_binding_revisions(&f, &revisions);
}

fn write_commit_object(f: &Fixture, tree: &str, message: &str) -> Oid {
    let raw = format!(
        "tree {tree}\nauthor fixture <fixture@example.invalid> 1 +0000\n\
         committer fixture <fixture@example.invalid> 1 +0000\n\n{message}"
    );
    f.repo
        .odb()
        .unwrap()
        .write(ObjectType::Commit, raw.as_bytes())
        .unwrap()
}

fn assert_semantic_refusal(f: &Fixture, revision: &str, object_kind: &str) {
    let verdict = f.json(revision, 4);
    assert_eq!(verdict["status"], "REFUSED");
    let error = verdict["error"].as_str().unwrap();
    assert!(
        error.contains(object_kind),
        "expected named {object_kind} admission: {error}"
    );
    assert!(
        error.contains("limit") || error.contains("exceed"),
        "expected semantic size refusal, not worker death: {error}"
    );
}

#[test]
fn oversized_initial_root_tree_is_refused_by_named_semantic_bound() {
    let f = Fixture::new();
    let blob = f.repo.blob(b"negative oversized metadata fixture").unwrap();
    let mut bytes = Vec::new();
    let mut index = 0;
    while bytes.len() <= 8 * 1024 * 1024 {
        bytes.extend_from_slice(format!("100644 n{index:07}\0").as_bytes());
        bytes.extend_from_slice(blob.as_bytes());
        index += 1;
    }
    // Synthetic invalid-for-admission Git objects are negative inputs only;
    // automatic receipt bytes and their actual source/manifest corpus stay intact.
    let tree = f
        .repo
        .odb()
        .unwrap()
        .write(ObjectType::Tree, &bytes)
        .unwrap();
    assert_semantic_refusal(&f, &tree.to_string(), "tree");
    let commit = write_commit_object(&f, &tree.to_string(), "oversized root tree");
    assert_semantic_refusal(&f, &commit.to_string(), "tree");
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

#[test]
fn oversized_resolved_commit_and_tag_messages_are_named_semantic_refusals() {
    let f = Fixture::new();
    let message = "x".repeat(1024 * 1024 + 1);
    let commit = write_commit_object(&f, &f.tree, &message);
    assert_semantic_refusal(&f, &commit.to_string(), "commit");
    let target = f.repo.head().unwrap().peel_to_commit().unwrap().id();
    let tag = format!(
        "object {target}\ntype commit\ntag oversized-message\n\
         tagger fixture <fixture@example.invalid> 1 +0000\n\n{message}"
    );
    let tag = f
        .repo
        .odb()
        .unwrap()
        .write(ObjectType::Tag, tag.as_bytes())
        .unwrap();
    assert_semantic_refusal(&f, &tag.to_string(), "tag");
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

#[test]
fn thirty_two_tag_hops_bind_but_thirty_three_are_refused() {
    let f = Fixture::new();
    let sig = Signature::now("tag-fixture", "fixture@example.invalid").unwrap();
    let mut target = f.repo.head().unwrap().target().unwrap();
    for index in 0..32 {
        let object = f.repo.find_object(target, None).unwrap();
        target = f
            .repo
            .tag(&format!("depth-{index:02}"), &object, &sig, "hop", false)
            .unwrap();
    }
    assert_binding_revisions(&f, &[target.to_string()]);
    let object = f.repo.find_object(target, None).unwrap();
    let thirty_three = f
        .repo
        .tag("depth-overflow", &object, &sig, "hop", false)
        .unwrap();
    assert_semantic_refusal(&f, &thirty_three.to_string(), "tag");
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

fn fixture_paths(root: &Path) -> std::collections::BTreeSet<PathBuf> {
    fn visit(base: &Path, root: &Path, found: &mut std::collections::BTreeSet<PathBuf>) {
        for entry in fs::read_dir(base).unwrap() {
            let entry = entry.unwrap();
            found.insert(entry.path().strip_prefix(root).unwrap().to_path_buf());
            if entry.file_type().unwrap().is_dir() {
                visit(&entry.path(), root, found);
            }
        }
    }
    let mut result = std::collections::BTreeSet::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn direct_worker_stdout_descriptor_is_refused_without_stdout_or_artifacts() {
    let f = Fixture::new();
    let before = fixture_paths(&f.root);
    let out = probe(f.command("HEAD").args(["--worker-fd", "1"]));
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "worker refusal must not corrupt stdout"
    );
    assert_eq!(fixture_paths(&f.root), before);
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

#[test]
fn direct_worker_memfd_frame_reports_enforced_kernel_limits_and_original_bindings() {
    let f = Fixture::new();
    let before = fixture_paths(&f.root);
    // The hidden entry installs its own hard limits; this invokes the actual
    // entry with its real anonymous channel, without extra resource flags.
    let fd = unsafe {
        libc::memfd_create(
            c"receipt-kernel-contract".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(fd >= 3, "{}", std::io::Error::last_os_error());
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut command = f.command("HEAD");
    command.args(["--json", "--worker-fd", &fd.to_string()]);
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = probe(&mut command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    assert_eq!(
        seals,
        libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
    );
    file.seek(SeekFrom::Start(0)).unwrap();
    let frame_limit = 16 * 1024 * 1024 + 4096;
    let mut bytes = Vec::new();
    (&mut file)
        .take(frame_limit + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= frame_limit as usize);
    let frame: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(frame["schema_version"], 1);
    assert!(frame["error"].is_null());
    assert_eq!(frame["verdict"]["status"], "PASS");
    assert_eq!(frame["verdict"]["tree_oid"], f.tree);
    let limits = &frame["limits"];
    assert!((1..=30).contains(&limits["cpu_seconds"].as_u64().unwrap()));
    assert!((1..=2 * 1024 * 1024 * 1024).contains(&limits["address_space_bytes"].as_u64().unwrap()));
    assert!((1..=frame_limit).contains(&limits["file_bytes"].as_u64().unwrap()));
    assert_eq!(limits["core_bytes"], 0);
    assert_eq!(fixture_paths(&f.root), before);
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

#[test]
fn inherited_kernel_file_ceiling_refuses_failed_private_result_channel() {
    let f = Fixture::new();
    let before = fixture_paths(&f.root);
    let mut command = f.command("HEAD");
    command.arg("--json");
    // Pipes are unaffected, but the private memfd cannot hold a complete frame.
    // Exercise actual kernel failure through the unchanged public CLI.
    unsafe {
        command.pre_exec(|| {
            for (resource, bytes) in [(libc::RLIMIT_FSIZE, 32), (libc::RLIMIT_CORE, 0)] {
                let limit = libc::rlimit {
                    rlim_cur: bytes,
                    rlim_max: bytes,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let output = probe(&mut command);
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let verdict: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(verdict["status"], "REFUSED");
    assert!(verdict.get("checks").is_none());
    assert_eq!(fixture_paths(&f.root), before);
    assert_eq!(fs::read(&f.receipt).unwrap(), RECEIPT);
}

#[test]
fn worker_reexecution_preserves_valid_leading_hyphen_receipt_path_bytes() {
    let f = Fixture::new();
    fs::write(f.root.join("repo/-receipt.json"), RECEIPT).unwrap();
    let mut command = Command::new(forge_binary::path());
    command
        .current_dir(f.root.join("repo"))
        .env_clear()
        .env("PATH", "")
        .env("HOME", &f.root)
        .args([
            "receipt",
            "verify",
            "--json",
            "--receipt=-receipt.json",
            "--tree=HEAD",
        ]);
    let output = probe(&mut command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let verdict: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(verdict["status"], "PASS");
    assert_eq!(verdict["receipt"], "-receipt.json");
    assert_eq!(
        fs::read(f.root.join("repo/-receipt.json")).unwrap(),
        RECEIPT
    );
}
