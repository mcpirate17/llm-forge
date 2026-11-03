//! Restart interpreter for the cooperative checkout-sync journal.
//! This module restores only recorded objects under the exclusive lease; it
//! retains the journal, recovery ref and displaced slots on every outcome.
use crate::{
    candidate_source::Source,
    checkout_sync_guards::{RecoveryIndex, RecoveryRefs},
    checkout_sync_index,
    checkout_sync_journal::{Event, Identity, Journal},
    checkout_sync_worktree::{self, Depot, Root, SyncLease},
};
use anyhow::{bail, ensure, Context, Result};
use git2::{Oid, Signature};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[derive(Clone)]
struct Header {
    root: String,
    root_device: u64,
    root_inode: u64,
    common_dir: String,
    common_device: u64,
    common_inode: u64,
    index_path: String,
    index_parent: (u64, u64),
    original_index: Option<Identity>,
    recovery_oid: String,
    branch: String,
    old: String,
    new: String,
    recovery_ref: String,
}
#[derive(Clone)]
struct PathIntent {
    path: String,
    parent: (u64, u64),
    slot: String,
    original: Option<Identity>,
    staged: Option<Identity>,
    blob: Option<String>,
}
#[derive(Clone)]
struct StageIntent {
    leaf: PathIntent,
    has_staged: bool,
    ready: bool,
}
#[derive(Clone)]
struct IndexIntent {
    parent: (u64, u64),
    original: Option<Identity>,
    blob: Option<String>,
    staged: Identity,
}
struct Plan {
    header: Header,
    paths: Vec<PathIntent>,
    depot: Option<(String, Option<(u64, u64)>)>,
    index: Option<IndexIntent>,
    has_ref: bool,
    completed: bool,
    recovered: bool,
    restored: BTreeSet<String>,
    incomplete_setup: Option<String>,
    recorded_slots: Vec<String>,
    guard_states: BTreeMap<String, String>,
    forward_guards: BTreeMap<String, String>,
    snapshot_pending: Option<String>,
}
fn valid_identity(identity: &Identity) -> bool {
    let kind = identity.mode & libc::S_IFMT;
    let valid_kind = matches!(kind, libc::S_IFREG | libc::S_IFLNK | libc::S_IFDIR);
    let limit = if kind == libc::S_IFLNK {
        4096
    } else {
        64 * 1024 * 1024
    };
    valid_kind
        && identity.length <= limit
        && identity.sha256.len() == 64
        && identity
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn subjects(plan: &Plan) -> Vec<String> {
    let mut out = Vec::new();
    if plan.index.is_some() {
        out.push("index".into());
    }
    out.extend(plan.paths.iter().rev().map(|p| format!("path:{}", p.path)));
    if plan.has_ref {
        out.push("ref".into());
    }
    out
}
fn parse(events: &[Event]) -> Result<Plan> {
    let mut validator = Validator::default();
    for event in events {
        validator.push(event)?;
    }
    validator.into_plan()
}
#[derive(Default)]
pub(crate) struct Validator {
    parser: Option<Parser>,
}
impl Validator {
    pub(crate) fn permits_preimage(&self) -> bool {
        self.parser.as_ref().is_some_and(|p| {
            !p.plan.completed
                && !p.plan.recovered
                && !p.rollback
                && p.pending.is_none()
                && !p.plan.has_ref
        })
    }
    pub(crate) fn push(&mut self, event: &Event) -> Result<()> {
        if let Some(parser) = &mut self.parser {
            parser.step(event)
        } else {
            self.parser = Some(start(event)?);
            Ok(())
        }
    }
    fn into_plan(self) -> Result<Plan> {
        let mut parser = self
            .parser
            .context("journal must begin with bound source")?;
        if parser
            .pending
            .as_deref()
            .is_some_and(|p| p.starts_with("stage:") || p == "depot")
        {
            parser.plan.incomplete_setup = parser.pending.clone();
        }
        parser.plan.recorded_slots = parser
            .stages
            .values()
            .map(|s| format!("{}:{}", s.leaf.path, s.leaf.slot))
            .collect();
        Ok(parser.plan)
    }
}
fn start(event: &Event) -> Result<Parser> {
    let Some(Event::Begin {
        root,
        root_device,
        root_inode,
        common_dir,
        common_device,
        common_inode,
        index_path,
        index_parent_device,
        index_parent_inode,
        original_index,
        recovery_oid,
        branch,
        old,
        new,
        target,
        recovery_ref,
        ..
    }) = Some(event)
    else {
        bail!("journal must begin with bound source")
    };
    ensure!(
        Path::new(root).is_absolute()
            && Path::new(common_dir).is_absolute()
            && Path::new(index_path).is_absolute(),
        "journal source paths must be absolute"
    );
    ensure!(
        branch.starts_with("refs/heads/")
            && git2::Reference::is_valid_name(branch)
            && target.starts_with("refs/")
            && git2::Reference::is_valid_name(target)
            && recovery_ref.starts_with("refs/snapshots/")
            && git2::Reference::is_valid_name(recovery_ref),
        "invalid journal refs"
    );
    ensure!(
        original_index
            .as_ref()
            .is_none_or(|i| valid_identity(i) && i.mode & libc::S_IFMT == libc::S_IFREG),
        "invalid original index authority"
    );
    Oid::from_str(old)?;
    Oid::from_str(new)?;
    if !recovery_oid.is_empty() {
        Oid::from_str(recovery_oid)?;
    }
    let plan = Plan {
        header: Header {
            root: root.clone(),
            root_device: *root_device,
            root_inode: *root_inode,
            common_dir: common_dir.clone(),
            common_device: *common_device,
            common_inode: *common_inode,
            index_path: index_path.clone(),
            index_parent: (*index_parent_device, *index_parent_inode),
            original_index: original_index.clone(),
            recovery_oid: recovery_oid.clone(),
            branch: branch.clone(),
            old: old.clone(),
            new: new.clone(),
            recovery_ref: recovery_ref.clone(),
        },
        paths: vec![],
        depot: None,
        index: None,
        has_ref: false,
        completed: false,
        recovered: false,
        restored: BTreeSet::new(),
        incomplete_setup: None,
        recorded_slots: Vec::new(),
        guard_states: BTreeMap::new(),
        forward_guards: BTreeMap::new(),
        snapshot_pending: None,
    };
    Ok(Parser {
        plan,
        pending: None,
        rollback: false,
        rollback_pending: None,
        rollback_order: Vec::new(),
        rollback_cursor: 0,
        seen: BTreeSet::new(),
        stages: BTreeMap::new(),
        slots_seen: BTreeSet::new(),
    })
}

struct Parser {
    plan: Plan,
    pending: Option<String>,
    rollback: bool,
    rollback_pending: Option<String>,
    rollback_order: Vec<String>,
    rollback_cursor: usize,
    seen: BTreeSet<String>,
    stages: BTreeMap<String, StageIntent>,
    slots_seen: BTreeSet<String>,
}
impl Parser {
    fn step(&mut self, event: &Event) -> Result<()> {
        ensure!(
            !self.plan.completed && !self.plan.recovered,
            "record after terminal transaction"
        );
        match event {
            Event::ForwardAcquire { .. }
            | Event::ForwardHeld { .. }
            | Event::ForwardRelease { .. }
            | Event::ForwardReleased { .. }
                if !self.rollback =>
            {
                self.forward_guard_event(event)
            }
            Event::ObjectIntent { purpose } if !self.rollback => {
                ensure!(
                    !purpose.is_empty()
                        && purpose.len() <= 80
                        && self.forward_held("refs")
                        && self.forward_held("index"),
                    "invalid immutable object intent"
                );
                Ok(())
            }
            Event::BeforeSnapshot { reference, oid } if !self.rollback => {
                ensure!(
                    self.plan.header.recovery_oid.is_empty()
                        && self.plan.snapshot_pending.is_none()
                        && reference == &self.plan.header.recovery_ref,
                    "invalid snapshot intent"
                );
                ensure!(
                    self.forward_held("refs") && self.forward_held("index"),
                    "snapshot requires forward Git guards"
                );
                Oid::from_str(oid)?;
                self.plan.snapshot_pending = Some(oid.clone());
                Ok(())
            }
            Event::SnapshotReady { oid } if !self.rollback => {
                ensure!(
                    self.plan.snapshot_pending.as_ref() == Some(oid)
                        && self.plan.header.recovery_oid.is_empty(),
                    "snapshot completion without intent"
                );
                Oid::from_str(oid)?;
                self.plan.header.recovery_oid = oid.clone();
                self.plan.snapshot_pending = None;
                Ok(())
            }
            Event::BeforeDepot { .. } | Event::AfterDepot { .. } if !self.rollback => {
                ensure!(
                    !self.plan.header.recovery_oid.is_empty()
                        && self.plan.snapshot_pending.is_none(),
                    "snapshot not ready"
                );
                self.depot_event(event)
            }
            Event::BeforeStage { .. } | Event::AfterStage { .. } if !self.rollback => {
                self.stage_event(event)
            }
            Event::BeforePath { .. } | Event::AfterPath { .. } if !self.rollback => {
                self.path_event(event)
            }
            Event::BeforeIndex { .. }
            | Event::AfterIndex { .. }
            | Event::BeforeRef { .. }
            | Event::AfterRef { .. }
                if !self.rollback =>
            {
                self.git_event(event)
            }
            Event::GuardAcquire { .. }
            | Event::GuardHeld { .. }
            | Event::GuardRelease { .. }
            | Event::GuardReleased { .. } => self.guard_event(event),
            Event::BeforeRollback { .. } | Event::Rollback { .. } => self.rollback_event(event),
            Event::RecoveryComplete => self.recovery_complete(),
            Event::Complete if !self.rollback => {
                ensure!(
                    self.pending.is_none()
                        && self.plan.has_ref
                        && self.plan.forward_guards.values().all(|s| s == "released"),
                    "incomplete publication marked complete"
                );
                self.plan.completed = true;
                Ok(())
            }
            _ => bail!("invalid journal phase transition"),
        }
    }
    fn recovery_complete(&mut self) -> Result<()> {
        ensure!(
            self.pending.is_none()
                && self.plan.forward_guards.values().all(|v| v == "released")
                && !self.plan.header.recovery_oid.is_empty()
                && self.plan.snapshot_pending.is_none()
                && self.plan.guard_states.values().all(|v| v == "released")
                && self.rollback_pending.is_none()
                && subjects(&self.plan)
                    .iter()
                    .all(|subject| self.plan.restored.contains(subject)),
            "recovery marked complete with pending work"
        );
        self.plan.recovered = true;
        Ok(())
    }
    fn depot_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::BeforeDepot { name } => {
                ensure!(
                    self.pending.is_none()
                        && self.plan.depot.is_none()
                        && self.stages.is_empty()
                        && name.starts_with("checkout-sync-depot-")
                        && name.len() <= 255
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                    "invalid depot intent"
                );
                self.plan.depot = Some((name.clone(), None));
                self.pending = Some("depot".into());
            }
            Event::AfterDepot {
                name,
                device,
                inode,
            } => {
                ensure!(
                    self.pending.as_deref() == Some("depot")
                        && self.plan.depot.as_ref().is_some_and(|(n, _)| n == name),
                    "depot completion mismatch"
                );
                self.plan.depot = Some((name.clone(), Some((*device, *inode))));
                self.pending = None;
            }
            _ => bail!("invalid depot record"),
        }
        Ok(())
    }
    fn stage_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::BeforeStage {
                path,
                parent_device,
                parent_inode,
                slot,
                original,
                original_blob,
                has_staged,
            } => {
                ensure!(
                    self.pending.is_none()
                        && self.plan.depot.as_ref().is_some_and(|(_, id)| id.is_some())
                        && !self.stages.contains_key(path),
                    "invalid stage intent order"
                );
                ensure!(original.is_some() || *has_staged, "empty stage intent");
                self.validate_stage(path, slot, original.as_ref(), original_blob.as_deref())?;
                ensure!(
                    self.slots_seen.insert(slot.clone()),
                    "duplicate recovery slot"
                );
                self.stages.insert(
                    path.clone(),
                    StageIntent {
                        leaf: PathIntent {
                            path: path.clone(),
                            parent: (*parent_device, *parent_inode),
                            slot: slot.clone(),
                            original: original.clone(),
                            staged: None,
                            blob: original_blob.clone(),
                        },
                        has_staged: *has_staged,
                        ready: false,
                    },
                );
                self.pending = Some(format!("stage:{path}"));
            }
            Event::AfterStage { path, staged } => {
                ensure!(
                    self.pending.as_deref() == Some(format!("stage:{path}").as_str()),
                    "stage completion without intent"
                );
                let stage = self.stages.get_mut(path).context("missing stage intent")?;
                ensure!(
                    staged.is_some() == stage.has_staged,
                    "staged presence mismatch"
                );
                ensure!(
                    staged.as_ref().is_none_or(valid_identity),
                    "invalid staged worktree identity"
                );
                stage.leaf.staged = staged.clone();
                stage.ready = true;
                self.pending = None;
            }
            _ => bail!("invalid stage record"),
        }
        Ok(())
    }
    fn validate_stage(
        &self,
        path: &str,
        slot: &str,
        original: Option<&Identity>,
        original_blob: Option<&str>,
    ) -> Result<()> {
        if let Some(identity) = original {
            ensure!(
                valid_identity(identity),
                "invalid original worktree identity"
            );
            if identity.mode & libc::S_IFMT == libc::S_IFDIR {
                ensure!(original_blob.is_none(), "directory has unexpected blob");
            } else {
                ensure!(
                    original_blob == Some(slot),
                    "worktree preimage slot mismatch"
                );
            }
        } else {
            ensure!(original_blob.is_none(), "absent worktree entry has a blob");
        }
        crate::candidate_source::safe_path(path.as_bytes())?;
        let depot_name = &self.plan.depot.as_ref().context("depot missing")?.0;
        ensure!(
            path != depot_name.as_str() && !path.starts_with(&format!("{depot_name}/")),
            "journal target overlaps recovery depot"
        );
        ensure!(
            !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "journal targets Git metadata"
        );
        ensure!(
            slot.starts_with("checkout-sync-slot-")
                && slot.len() <= 255
                && slot
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                && Path::new(path).file_name().is_some_and(|name| name != slot),
            "invalid journal recovery slot"
        );
        Ok(())
    }
    fn path_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::BeforePath {
                path,
                parent_device,
                parent_inode,
                slot,
                original,
                original_blob,
                staged,
            } => {
                if !self.plan.forward_guards.is_empty() {
                    ensure!(
                        self.forward_held("refs") && self.forward_held("index"),
                        "publication requires forward refs and index guards"
                    );
                }
                ensure!(
                    self.pending.is_none()
                        && self.plan.index.is_none()
                        && self.seen.insert(path.clone()),
                    "invalid path intent order"
                );
                let prepared = self
                    .stages
                    .get(path)
                    .context("path has no prepared stage")?;
                ensure!(
                    prepared.ready
                        && prepared.leaf.parent == (*parent_device, *parent_inode)
                        && prepared.leaf.slot == *slot
                        && prepared.leaf.original == *original
                        && prepared.leaf.blob == *original_blob
                        && prepared.leaf.staged == *staged,
                    "path intent differs from staged input"
                );
                self.plan.paths.push(prepared.leaf.clone());
                self.pending = Some(format!("path:{path}"));
            }
            Event::AfterPath { path, published } => {
                ensure!(
                    self.pending.as_deref() == Some(format!("path:{path}").as_str()),
                    "path completion without intent"
                );
                ensure!(
                    self.plan
                        .paths
                        .last()
                        .is_some_and(|p| &p.staged == published),
                    "path publication identity mismatch"
                );
                self.pending = None;
            }
            _ => bail!("invalid path record"),
        }
        Ok(())
    }
    fn git_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::BeforeIndex {
                parent_device,
                parent_inode,
                original,
                original_blob,
                staged,
            } => {
                if !self.plan.forward_guards.is_empty() {
                    ensure!(
                        self.forward_held("refs") && self.forward_held("index"),
                        "index publication requires forward Git guards"
                    );
                }
                ensure!(
                    self.pending.is_none()
                        && self.plan.index.is_none()
                        && self.seen.len() == self.stages.len(),
                    "invalid index intent order or unpublished staged path"
                );
                ensure!(
                    original.is_some() == original_blob.is_some()
                        && original == &self.plan.header.original_index
                        && (*parent_device, *parent_inode) == self.plan.header.index_parent,
                    "index preimage mismatch"
                );
                ensure!(
                    original.as_ref().is_none_or(valid_identity)
                        && valid_identity(staged)
                        && staged.mode & libc::S_IFMT == libc::S_IFREG,
                    "invalid index identity"
                );
                self.plan.index = Some(IndexIntent {
                    parent: (*parent_device, *parent_inode),
                    original: original.clone(),
                    blob: original_blob.clone(),
                    staged: staged.clone(),
                });
                self.pending = Some("index".into());
            }
            Event::AfterIndex { published } => {
                ensure!(
                    self.pending.as_deref() == Some("index")
                        && self
                            .plan
                            .index
                            .as_ref()
                            .is_some_and(|i| &i.staged == published),
                    "index completion mismatch"
                );
                self.pending = None;
            }
            Event::BeforeRef { branch, old, new } => {
                if !self.plan.forward_guards.is_empty() {
                    ensure!(
                        self.forward_held("refs") && self.forward_held("index"),
                        "ref publication requires both Git guards"
                    );
                }
                ensure!(
                    self.pending.is_none()
                        && self.plan.index.is_some()
                        && !self.plan.has_ref
                        && branch == &self.plan.header.branch
                        && old == &self.plan.header.old
                        && new == &self.plan.header.new,
                    "invalid ref intent"
                );
                self.plan.has_ref = true;
                self.pending = Some("ref".into());
            }
            Event::AfterRef { branch, new } => {
                ensure!(
                    self.pending.as_deref() == Some("ref")
                        && branch == &self.plan.header.branch
                        && new == &self.plan.header.new,
                    "ref completion mismatch"
                );
                self.pending = None;
            }
            _ => bail!("invalid Git record"),
        }
        Ok(())
    }
    fn forward_held(&self, subject: &str) -> bool {
        self.plan.forward_guards.get(subject).map(String::as_str) == Some("held")
    }
    fn forward_guard_event(&mut self, event: &Event) -> Result<()> {
        let (subject, from, to) = match event {
            Event::ForwardAcquire { subject } => (subject, "released", "acquiring"),
            Event::ForwardHeld { subject } => (subject, "acquiring", "held"),
            Event::ForwardRelease { subject } => (subject, "held", "releasing"),
            Event::ForwardReleased { subject } => (subject, "releasing", "released"),
            _ => unreachable!(),
        };
        ensure!(
            subject == "refs" || subject == "index",
            "invalid forward guard"
        );
        let state = self
            .plan
            .forward_guards
            .get(subject)
            .map(String::as_str)
            .unwrap_or("released");
        ensure!(state == from, "invalid forward guard phase");
        if subject == "index" && to == "acquiring" {
            ensure!(
                self.forward_held("refs"),
                "forward index guard requires refs"
            );
        }
        if subject == "refs" && to == "releasing" && self.forward_held("index") {
            ensure!(
                self.pending.as_deref() == Some("ref"),
                "release forward index first"
            );
        }
        self.plan.forward_guards.insert(subject.clone(), to.into());
        Ok(())
    }
    fn guard_event(&mut self, event: &Event) -> Result<()> {
        let (subject, from, to) = match event {
            Event::GuardAcquire { subject } => (subject, "released", "acquiring"),
            Event::GuardHeld { subject } => (subject, "acquiring", "held"),
            Event::GuardRelease { subject } => (subject, "held", "releasing"),
            Event::GuardReleased { subject } => (subject, "releasing", "released"),
            _ => unreachable!(),
        };
        ensure!(
            subject == "refs" || subject == "index",
            "invalid recovery guard"
        );
        ensure!(
            self.rollback_pending.is_none()
                || (subject == "refs"
                    && (self.rollback_pending.as_deref() == Some("ref")
                        || self.rollback_pending.as_deref() == Some("index")))
                || (subject == "index" && self.rollback_pending.as_deref() == Some("index")),
            "guard transition during rollback"
        );
        if !self.rollback {
            ensure!(
                !self
                    .pending
                    .as_deref()
                    .is_some_and(|p| p.starts_with("stage:") || p == "depot"),
                "uncertain setup"
            );
            self.rollback_order = subjects(&self.plan);
            self.rollback = true;
            self.pending = None;
        }
        let state = self
            .plan
            .guard_states
            .get(subject)
            .map(String::as_str)
            .unwrap_or("released");
        ensure!(state == from, "invalid recovery guard phase");
        if subject == "index" && to == "acquiring" {
            ensure!(
                self.plan.guard_states.get("refs").map(String::as_str) == Some("held"),
                "index guard requires held refs"
            );
        }
        if subject == "refs"
            && to == "releasing"
            && self.plan.guard_states.get("index").map(String::as_str) == Some("held")
        {
            ensure!(
                self.rollback_pending.as_deref() == Some("ref"),
                "release index guard first"
            );
        }
        self.plan.guard_states.insert(subject.clone(), to.into());
        Ok(())
    }
    fn rollback_event(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::BeforeRollback { subject } => {
                ensure!(
                    !self
                        .pending
                        .as_deref()
                        .is_some_and(|p| p.starts_with("stage:") || p == "depot"),
                    "unrecorded staged inode requires manual recovery"
                );
                if !self.rollback {
                    self.rollback_order = subjects(&self.plan);
                }
                self.rollback = true;
                self.pending = None;
                let next = self.rollback_order.get(self.rollback_cursor);
                ensure!(
                    next.map(String::as_str) == Some(subject.as_str())
                        && self.rollback_pending.as_ref().is_none_or(|s| s == subject),
                    "rollback order or subject mismatch"
                );
                if subject == "ref" {
                    ensure!(
                        self.plan.guard_states.get("refs").map(String::as_str) == Some("held")
                            && self.plan.guard_states.get("index").map(String::as_str)
                                == Some("held"),
                        "ref rollback requires both Git guards"
                    );
                }
                if subject.starts_with("path:") {
                    ensure!(
                        self.plan.guard_states.get("refs").map(String::as_str) == Some("held")
                            && self.plan.guard_states.get("index").map(String::as_str)
                                == Some("held"),
                        "path rollback requires both Git guards"
                    );
                }
                if subject == "index" {
                    ensure!(
                        self.plan.guard_states.get("refs").map(String::as_str) == Some("held"),
                        "index rollback requires ref guard"
                    );
                }
                self.rollback_pending = Some(subject.clone());
            }
            Event::Rollback {
                subject, restored, ..
            } => {
                ensure!(
                    self.rollback && *restored && self.rollback_pending.as_ref() == Some(subject),
                    "rollback completion without matching intent"
                );
                self.plan.restored.insert(subject.clone());
                self.rollback_cursor += 1;
                self.rollback_pending = None;
            }
            _ => bail!("invalid rollback record"),
        }
        Ok(())
    }
}

fn verify_source(plan: &Plan) -> Result<Source> {
    let source = Source::open(Path::new(&plan.header.root))?;
    ensure!(
        source.root() == Path::new(&plan.header.root)
            && source.common_dir() == Path::new(&plan.header.common_dir),
        "recovery source path changed"
    );
    let root = std::fs::metadata(source.root())?;
    let common = std::fs::metadata(source.common_dir())?;
    ensure!(
        (root.dev(), root.ino()) == (plan.header.root_device, plan.header.root_inode)
            && (common.dev(), common.ino())
                == (plan.header.common_device, plan.header.common_inode),
        "recovery source identity changed"
    );
    let git_dir = source.repository().path().canonicalize()?;
    ensure!(
        Path::new(&plan.header.index_path) == git_dir.join("index"),
        "recovery index differs from actual repository index"
    );
    ensure!(
        plan.header.recovery_ref.starts_with("refs/snapshots/")
            && git2::Reference::is_valid_name(&plan.header.recovery_ref),
        "invalid recovery ref"
    );
    let index_parent = std::fs::metadata(
        Path::new(&plan.header.index_path)
            .parent()
            .context("index parent missing")?,
    )?;
    ensure!(
        (index_parent.dev(), index_parent.ino()) == plan.header.index_parent,
        "index parent changed"
    );
    let recovery = Oid::from_str(&plan.header.recovery_oid)?;
    ensure!(
        source
            .repository()
            .find_reference(&plan.header.recovery_ref)?
            .target()
            == Some(recovery),
        "recovery ref changed"
    );
    Ok(source)
}

/// Reacquire the exclusive cooperative lease and restore in reverse order.
/// Errors leave the journal and all surviving slots/ref intact for inspection.
#[derive(Debug)]
pub(crate) enum RecoveryOutcome {
    JournalMarkedComplete,
    JournalMarkedRecovered,
    Recovered {
        depot: Option<String>,
        recorded_slots: Vec<String>,
    },
}
pub(crate) fn recover(
    dir: &Path,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<RecoveryOutcome> {
    let initial = crate::checkout_sync_journal::read_records(dir)?;
    let initial_plan = parse(&initial)?;
    ensure!(
        !initial_plan.header.recovery_oid.is_empty() && initial_plan.snapshot_pending.is_none(),
        "snapshot publication incomplete; recovery artifacts retained"
    );
    ensure!(
        initial_plan
            .forward_guards
            .values()
            .all(|v| v == "released"),
        "forward guard interrupted; preserve locks and recovery artifacts: {:?}",
        initial_plan.forward_guards
    );
    let initial_source = verify_source(&initial_plan)?;
    let lease = SyncLease::acquire(initial_source.root(), cancel)?;
    let (mut journal, events) = Journal::reopen(dir)?;
    ensure!(events == initial, "recovery journal changed before lease");
    let plan = parse(&events)?;
    let source = verify_source(&plan)?;
    if plan.completed {
        return Ok(RecoveryOutcome::JournalMarkedComplete);
    }
    if plan.recovered {
        return Ok(RecoveryOutcome::JournalMarkedRecovered);
    }
    ensure!(
        plan.incomplete_setup.is_none(),
        "unrecorded staging/depot inode requires manual recovery: phase={:?} depot={:?} slots={:?}",
        plan.incomplete_setup,
        plan.depot,
        plan.recorded_slots
    );
    let root = Root::open(source.root())?;
    let depot = match &plan.depot {
        Some((name, Some(identity))) => Some(Depot::open(&root, &lease, name, *identity)?),
        None => None,
        _ => bail!("unrecorded depot requires manual recovery"),
    };
    ensure!(
        plan.guard_states.values().all(|s| s == "released"),
        "interrupted recovery guard requires manual lock inspection; journal retained: {:?}",
        plan.guard_states
    );
    let old = Oid::from_str(&plan.header.old)?;
    let new = Oid::from_str(&plan.header.new)?;
    guard_record(&mut journal, "refs", "acquire")?;
    let refs =
        RecoveryRefs::acquire_for_restore(source.repository(), &plan.header.branch, old, new)?;
    sync_ref_dirs(&plan)?;
    guard_record(&mut journal, "refs", "held")?;
    if !plan.has_ref || plan.restored.contains("ref") {
        ensure!(
            source
                .repository()
                .find_reference(&plan.header.branch)?
                .target()
                == Some(old),
            "branch is not original"
        );
    }
    recover_with_refs(plan, root, depot, lease, journal, refs, cancel)
}

fn sync_index_dir(plan: &Plan) -> Result<()> {
    std::fs::File::open(
        Path::new(&plan.header.index_path)
            .parent()
            .context("index parent missing")?,
    )?
    .sync_all()?;
    Ok(())
}
fn sync_ref_dirs(plan: &Plan) -> Result<()> {
    sync_index_dir(plan)?;
    let branch = Path::new(&plan.header.common_dir).join(&plan.header.branch);
    std::fs::File::open(branch.parent().context("ref parent missing")?)?.sync_all()?;
    Ok(())
}
fn guard_record(journal: &mut Journal, subject: &str, phase: &str) -> Result<()> {
    let subject = subject.to_owned();
    let event = match phase {
        "acquire" => Event::GuardAcquire { subject },
        "held" => Event::GuardHeld { subject },
        "release" => Event::GuardRelease { subject },
        "released" => Event::GuardReleased { subject },
        _ => bail!("unknown guard phase"),
    };
    journal.append(event)
}

fn recover_with_refs(
    plan: Plan,
    root: Root,
    depot: Option<Depot>,
    lease: SyncLease,
    mut journal: Journal,
    refs: RecoveryRefs<'_>,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<RecoveryOutcome> {
    if let Some(index) = &plan.index {
        if !plan.restored.contains("index") {
            refs.verify()?;
            recover_index(&plan, &lease, &mut journal, index, cancel).with_context(|| {
                format!("index recovery retained at {}", journal.path().display())
            })?;
        }
    }
    refs.verify()?;
    lease.verify()?;
    guard_record(&mut journal, "index", "acquire")?;
    let index_guard = RecoveryIndex::acquire(
        Path::new(&plan.header.index_path),
        plan.header.original_index.as_ref(),
    )?;
    sync_index_dir(&plan)?;
    guard_record(&mut journal, "index", "held")?;
    for item in plan.paths.iter().rev() {
        if plan.restored.contains(&format!("path:{}", item.path)) {
            continue;
        }
        cancel()?;
        refs.verify()?;
        index_guard.verify()?;
        lease.verify()?;
        checkout_sync_worktree::rollback_intent(
            &root,
            depot.as_ref().context("worktree recovery depot missing")?,
            &lease,
            &mut journal,
            checkout_sync_worktree::RecoveryLeaf {
                path: &item.path,
                parent_identity: item.parent,
                slot: &item.slot,
                original: item.original.clone(),
                staged: item.staged.clone(),
            },
        )
        .with_context(|| {
            format!(
                "worktree recovery retained: path={} depot_slot={} journal={}",
                item.path,
                item.slot,
                journal.path().display()
            )
        })?;
    }
    refs.verify()?;
    index_guard.verify()?;
    lease.verify()?;
    if plan.has_ref && !plan.restored.contains("ref") {
        cancel()?;
        journal.append(Event::BeforeRollback {
            subject: "ref".into(),
        })?;
        guard_record(&mut journal, "refs", "release")?;
        let signature = Signature::now("forge-checkout-sync", "forge-checkout-sync@localhost")?;
        refs.restore(Oid::from_str(&plan.header.old)?, &signature)?;
        sync_ref_dirs(&plan)?;
        guard_record(&mut journal, "refs", "released")?;
        journal.append(Event::Rollback {
            subject: "ref".into(),
            restored: true,
            detail: "conditional ref restore".into(),
        })?;
        guard_record(&mut journal, "index", "release")?;
        drop(index_guard);
        sync_index_dir(&plan)?;
        guard_record(&mut journal, "index", "released")?;
    } else {
        guard_record(&mut journal, "index", "release")?;
        drop(index_guard);
        sync_index_dir(&plan)?;
        guard_record(&mut journal, "index", "released")?;
        guard_record(&mut journal, "refs", "release")?;
        drop(refs);
        sync_ref_dirs(&plan)?;
        guard_record(&mut journal, "refs", "released")?;
    }
    journal.append(Event::RecoveryComplete)?;
    Ok(RecoveryOutcome::Recovered {
        depot: plan.depot.as_ref().map(|(name, _)| name.clone()),
        recorded_slots: plan.recorded_slots,
    })
}
fn recover_index(
    plan: &Plan,
    lease: &SyncLease,
    journal: &mut Journal,
    intent: &IndexIntent,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    cancel()?;
    lease.verify()?;
    let index = Path::new(&plan.header.index_path);
    let parent = index.parent().context("index parent missing")?;
    let m = std::fs::metadata(parent)?;
    ensure!((m.dev(), m.ino()) == intent.parent, "index parent changed");
    let bytes = match (&intent.original, &intent.blob) {
        (Some(identity), Some(slot)) => Some(journal.read_blob(slot, &identity.sha256)?),
        (None, None) => None,
        _ => bail!("index preimage missing"),
    };
    guard_record(journal, "index", "acquire")?;
    checkout_sync_index::remove_owned_stale_lock(index, &intent.staged)?;
    let guard =
        RecoveryIndex::acquire_for_restore(index, intent.original.as_ref(), &intent.staged)?;
    sync_index_dir(plan)?;
    guard_record(journal, "index", "held")?;
    journal.append(Event::BeforeRollback {
        subject: "index".into(),
    })?;
    cancel()?;
    lease.verify()?;
    guard.verify()?;
    guard_record(journal, "index", "release")?;
    guard.restore(intent.original.as_ref(), &intent.staged, bytes.as_deref())?;
    sync_index_dir(plan)?;
    guard_record(journal, "index", "released")?;
    lease.verify()?;
    journal.append(Event::Rollback {
        subject: "index".into(),
        restored: true,
        detail: "conditional index restore".into(),
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "checkout_sync_recovery_tests.rs"]
mod tests;
