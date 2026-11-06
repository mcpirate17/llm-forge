//! Operator capabilities. Model arguments can select a capability, never widen it.
use crate::cgroup::Limits;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub source_root: PathBuf,
    pub write_paths: Vec<PathBuf>,
    pub read_only: Vec<PathBuf>,
    pub programs: BTreeMap<String, Program>,
    #[serde(default)]
    pub code_indexes: BTreeMap<String, CodeIndex>,
    #[serde(default)]
    pub context: Option<ContextBudget>,
    #[serde(default)]
    pub workspace: Option<super::workspace::BriefCapability>,
    #[serde(default)]
    pub memory_indexes: BTreeMap<String, super::workspace::MemoryCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbound_mcp: Option<OutboundCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_report: Option<TestReportCapability>,
    pub aggregate: Limits,
    pub session: Limits,
    pub max_sessions: usize,
    pub max_retained_sessions: usize,
    pub max_tasks: usize,
    pub lifetime_seconds: u64,
    pub session_seconds: u64,
    pub retention_seconds: u64,
    pub replay_bytes: usize,
    pub log_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestReportCapability {
    pub executable: OutboundPin,
    pub read_only: Vec<PathBuf>,
    pub resources: Limits,
    pub max_wall_seconds: u64,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundPin {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundCapability {
    pub public_name: String,
    pub remote_name: String,
    pub executable: OutboundPin,
    pub fixed_arguments: Vec<String>,
    pub pinned_inputs: Vec<OutboundPin>,
    pub read_only: Vec<PathBuf>,
    pub input_schema_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_json: Option<String>,
    pub resources: Limits,
    pub max_wall_seconds: u64,
}
impl OutboundCapability {
    pub(crate) fn bind(
        &self,
        policy: &Policy,
        state: &Path,
    ) -> Result<crate::mutation_execution_grants::OutboundBindings> {
        arguments(&self.fixed_arguments)?;
        ensure!(
            self.public_name.starts_with("outbound_")
                && self.public_name.len() <= 128
                && self
                    .public_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid outbound public name"
        );
        ensure!(
            (1..=policy.lifetime_seconds).contains(&self.max_wall_seconds),
            "invalid outbound lifetime"
        );
        let r = self.resources;
        ensure!(
            (10..=1_024_000).contains(&r.cpu_millis)
                && (16..=1_048_576).contains(&r.memory_mib)
                && (1..=4096).contains(&r.processes),
            "invalid outbound resources"
        );
        crate::outbound_mcp_protocol::OutboundProtocol::new(
            &self.remote_name,
            &self.input_schema_json,
            self.output_schema_json.as_deref(),
        )?;
        let pins = self
            .pinned_inputs
            .iter()
            .map(|p| (p.path.clone(), p.sha256.clone()))
            .collect::<Vec<_>>();
        crate::mutation_execution_grants::OutboundBindings::capture(
            &self.executable.path,
            &self.executable.sha256,
            &self.read_only,
            &pins,
            &policy.source_root,
            state,
            &policy.read_only,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub executable: PathBuf,
    pub fixed_arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    /// Builds needing writable outputs use scratch; source remains read-only.
    pub working_directory: WorkingDirectory,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkingDirectory {
    Source,
    Scratch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodeIndex {
    pub directory: PathBuf,
    pub max_rows: usize,
    pub max_mapped_mib: u64,
    pub readers: usize,
    pub embedding: Option<crate::embedding::Config>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBudget {
    pub tokenizer: crate::tokenizer::Config,
    pub max_input_bytes: usize,
    pub max_tokens: usize,
    pub recovery_bytes: u64,
}
impl ContextBudget {
    fn reserve_mib(&self) -> Result<u64> {
        self.tokenizer.validate()?;
        ensure!(
            (128..=262144).contains(&self.max_input_bytes)
                && (1..=262144).contains(&self.max_tokens)
                && (1..=4 * 1024 * 1024 * 1024).contains(&self.recovery_bytes),
            "invalid context capability bounds"
        );
        let metadata = std::fs::symlink_metadata(&self.tokenizer.path)?;
        ensure!(
            metadata.is_file() && metadata.len() <= crate::tokenizer::MAX_ARTIFACT_BYTES as u64,
            "tokenizer artifact must be a bounded regular file"
        );
        // One context worker at a time. Reserve artifact loading/model expansion
        // plus bounded encoding, projection and recovery scratch. The cgroup is
        // the enforced ceiling; custom expanding normalizers remain operator-trusted.
        Ok(128 + 16 * metadata.len().div_ceil(1024 * 1024))
    }
}
impl CodeIndex {
    pub fn limits(&self) -> Result<crate::code_index::ReadLimits> {
        let limits = crate::code_index::ReadLimits {
            max_rows: self.max_rows,
            max_mapped_bytes: self
                .max_mapped_mib
                .checked_mul(1024 * 1024)
                .context("mapped index limit overflow")?,
        };
        limits.validate()?;
        Ok(limits)
    }
    fn reserve_mib(&self) -> Result<u64> {
        ensure!(
            (1..=4).contains(&self.readers) && (1..=8192).contains(&self.max_mapped_mib),
            "invalid index reader capacity"
        );
        self.limits()?;
        // Each independently pinned reader can retain a different generation.
        // Reserve its complete mapping, visit/frontier/result scratch and SQLite cache.
        Ok(self.readers as u64
            * (self.max_mapped_mib
                + 8
                + (20 * self.max_rows as u64 + 256 * 1024).div_ceil(1024 * 1024)))
    }
}

impl Policy {
    pub fn validate(mut self, state: &Path) -> Result<Self> {
        ensure!(self.version == 1, "unsupported harness policy version");
        ensure!(
            self.source_root.is_absolute(),
            "source root must be absolute"
        );
        self.source_root = self.source_root.canonicalize()?;
        ensure!(
            self.source_root.is_dir() && self.source_root.parent().is_some(),
            "invalid source root"
        );
        self.validate_limits()?;
        ensure!(
            self.write_paths.len() <= 4096
                && self.read_only.len() <= 120
                && self.programs.len() <= 64,
            "capability count exceeds bounds"
        );
        for path in &self.write_paths {
            source_path(path)?;
        }
        self.write_paths.sort();
        ensure!(
            !self.write_paths.windows(2).any(|p| p[0] == p[1]),
            "duplicate write capability"
        );
        let state = state
            .canonicalize()
            .context("state directory must already exist")?;
        disjoint(&self.source_root, &state)?;
        for path in &mut self.read_only {
            ensure!(path.is_absolute(), "read capability must be absolute");
            *path = path.canonicalize()?;
            disjoint(path, &state)?;
        }
        for (name, program) in &mut self.programs {
            crate::task_store::validate_id(name)?;
            ensure!(
                program.executable.is_absolute(),
                "executable must be absolute"
            );
            program.executable = program.executable.canonicalize()?;
            ensure!(
                program.executable.is_file(),
                "executable must be a regular file"
            );
            disjoint(&program.executable, &state)?;
            arguments(&program.fixed_arguments)?;
            crate::sandbox::validate_environment(&program.environment)?;
        }
        if let Some(capability) = &self.outbound_mcp {
            capability.bind(&self, &state)?;
        }
        if let Some(capability) = &self.test_report {
            crate::harness::test_report::validate(capability, &self, &state)?;
        }
        self.validate_indexes(&state)?;
        super::workspace::validate(&self, &state)?;
        Ok(self)
    }

    fn validate_limits(&self) -> Result<()> {
        ensure!(
            (1..=32).contains(&self.max_sessions)
                && (self.max_sessions..=256).contains(&self.max_retained_sessions)
                && (self.max_retained_sessions..=4096).contains(&self.max_tasks),
            "invalid session capacity"
        );
        ensure!(
            (1..=86400).contains(&self.lifetime_seconds)
                && (1..=self.lifetime_seconds).contains(&self.session_seconds)
                && (1..=3600).contains(&self.retention_seconds),
            "invalid lifecycle bounds"
        );
        ensure!(
            (1..=4 * 1024 * 1024).contains(&self.replay_bytes)
                && self.log_bytes <= 64 * 1024 * 1024,
            "invalid output limits"
        );
        ensure!(
            (10..=1_024_000).contains(&self.session.cpu_millis)
                && (16..=1_048_576).contains(&self.session.memory_mib)
                && (1..=4096).contains(&self.session.processes),
            "invalid session resources"
        );
        ensure!(
            self.aggregate.cpu_millis <= 1_024_000
                && self.aggregate.memory_mib <= 1_048_576
                && self.aggregate.processes <= 4096,
            "invalid aggregate resources"
        );
        if let Some(capability) = &self.outbound_mcp {
            let r = capability.resources;
            ensure!(
                (10..=1_024_000).contains(&r.cpu_millis)
                    && (16..=1_048_576).contains(&r.memory_mib)
                    && (1..=4096).contains(&r.processes),
                "invalid outbound resources"
            );
        }
        if let Some(capability) = &self.test_report {
            let r = capability.resources;
            ensure!(
                (10..=1_024_000).contains(&r.cpu_millis)
                    && (16..=1_048_576).contains(&r.memory_mib)
                    && (1..=4096).contains(&r.processes)
                    && (1..=20).contains(&capability.max_wall_seconds)
                    && capability.max_wall_seconds <= self.lifetime_seconds
                    && (512..=262144).contains(&capability.max_output_bytes),
                "invalid test report capability bounds"
            );
        }
        // Cover bounded request/reply buffers, retained replay and native workers.
        let supervisor_mib = 384
            + self
                .outbound_mcp
                .as_ref()
                .map_or(0, |c| c.resources.memory_mib + 20)
            + self
                .test_report
                .as_ref()
                .map_or(0, |c| c.resources.memory_mib + 20)
            + self.index_reservation()?
            + super::workspace::reservation(self)?
            + self
                .context
                .as_ref()
                .map(ContextBudget::reserve_mib)
                .transpose()?
                .unwrap_or(0)
            + (self.max_retained_sessions * self.replay_bytes).div_ceil(1024 * 1024) as u64;
        ensure!(
            u64::from(self.aggregate.cpu_millis) >= u64::from(self.session.cpu_millis) + self.outbound_mcp.as_ref().map_or(0, |c| u64::from(c.resources.cpu_millis)) + self.test_report.as_ref().map_or(0, |c| u64::from(c.resources.cpu_millis))
                && self.aggregate.memory_mib
                    >= supervisor_mib + self.session.memory_mib * self.max_sessions as u64
                && u64::from(self.aggregate.processes)
                    >= 16 + u64::from(self.session.processes) * self.max_sessions as u64 + self.outbound_mcp.as_ref().map_or(0, |c| u64::from(c.resources.processes)) + self.test_report.as_ref().map_or(0, |c| u64::from(c.resources.processes)),
            "aggregate budget must cover all sessions plus {supervisor_mib} MiB and 16 supervisor tasks"
        );
        Ok(())
    }

    pub fn permit_write(&self, path: &Path) -> Result<()> {
        source_path(path)?;
        ensure!(
            self.write_paths.binary_search(&path.to_path_buf()).is_ok(),
            "path lacks an operator write capability"
        );
        Ok(())
    }
}

impl Policy {
    fn index_reservation(&self) -> Result<u64> {
        ensure!(
            self.code_indexes.len() <= 8,
            "too many code-index capabilities"
        );
        let mut total = 0;
        let mut readers = 0;
        for index in self.code_indexes.values() {
            total += index.reserve_mib()?;
            readers += index.readers;
        }
        ensure!(readers <= 16, "too many code-index readers");
        Ok(total)
    }
    fn validate_indexes(&mut self, state: &Path) -> Result<()> {
        for (name, index) in &mut self.code_indexes {
            crate::task_store::validate_id(name)?;
            index.directory = super::state::owned_directory(&index.directory)?;
            disjoint(&self.source_root, &index.directory)?;
            disjoint(&index.directory, state)?;
            for path in &self.read_only {
                disjoint(path, &index.directory)?;
            }
            for program in self.programs.values() {
                disjoint(&program.executable, &index.directory)?;
            }
            if let Some(embedding) = &index.embedding {
                embedding.validate()?;
            }
        }
        Ok(())
    }
}

pub fn arguments(args: &[String]) -> Result<()> {
    ensure!(
        args.len() <= 128
            && args.iter().all(|s| s.len() <= 8192 && !s.contains('\0'))
            && args.iter().map(String::len).sum::<usize>() <= 65536,
        "argument count/size/NUL limit exceeded"
    );
    Ok(())
}

pub fn source_path(path: &Path) -> Result<()> {
    crate::source_file::validate_relative(path)?;
    ensure!(
        !path.starts_with(".agents") && !path.starts_with(".forge"),
        "internal control paths are not source capabilities"
    );
    Ok(())
}

fn disjoint(capability: &Path, state: &Path) -> Result<()> {
    ensure!(
        !state.starts_with(capability) && !capability.starts_with(state),
        "state must be outside source/read/executable capabilities: {}",
        capability.display()
    );
    Ok(())
}
