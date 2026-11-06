//! Fixed standalone build definition and held executable for confined Mull.
use crate::{
    candidate_inputs::{BoundFile, DirectoryBinding},
    mutation_campaign::Campaign,
    mutation_execution_grants::Grants,
    mutation_profile_inputs,
};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) struct Inputs {
    pub shared: mutation_profile_inputs::Inputs,
    source: String,
    driver: String,
}

#[cfg(test)]
#[path = "mutation_mull_runtime_tests.rs"]
mod tests;
impl Inputs {
    pub(crate) fn capture(campaign: &Campaign, root: &Path) -> Result<Self> {
        ensure!(
            campaign.language == "cpp"
                && campaign.mutation_engine == "mull"
                && campaign.option_text("execution_profile").as_deref() == Some("mull-confined-v1")
                && campaign.generated_value.is_none()
                && campaign.options.len() == 1,
            "Mull requires the verified standalone profile"
        );
        ensure!(
            campaign.source.len() == 1
                && campaign.source_sha256.len() == 1
                && campaign.source_sha256.contains_key(&campaign.source[0])
                && campaign.test_sha256.len() == 1
                && campaign.test_argv == ["forge-mull-test"]
                && campaign.jobs == 1
                && campaign.seed == 0
                && campaign.environment.is_empty()
                && campaign.operators.is_empty()
                && campaign.exclude.is_empty(),
            "invalid standalone Mull scope or execution options"
        );
        let source = campaign.source[0].clone();
        let driver = campaign.test_sha256.keys().next().unwrap().clone();
        for path in [&source, &driver] {
            conductor_native::mutation_plan::validate_mull_path(path)
                .map_err(anyhow::Error::msg)?;
        }
        ensure!(
            source != driver && source.ends_with(".c") == driver.ends_with(".c"),
            "Mull requires a distinct test driver in the source language"
        );
        Ok(Self {
            shared: mutation_profile_inputs::Inputs::capture(campaign, root)?,
            source,
            driver,
        })
    }
}

/// Only simple literal absolute paths enter the fixed CMake language.
fn literal(path: &Path) -> Result<&str> {
    let value = path.to_str().context("Mull path must be UTF-8")?;
    ensure!(
        path.is_absolute()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b)),
        "Mull build paths must be literal ASCII paths without whitespace or metacharacters"
    );
    Ok(value)
}
fn definition(inputs: &Inputs, worktree: &Path, grants: &Grants) -> Result<String> {
    let source_path = worktree.join(&inputs.source);
    let driver_path = worktree.join(&inputs.driver);
    let source = literal(&source_path)?;
    let driver = literal(&driver_path)?;
    let clang = literal(grants.program("clang")?)?;
    let ninja = literal(grants.program("ninja")?)?;
    let linker = literal(grants.program("ld")?)?;
    let plugin = literal(grants.mull_toolchain()?.plugin())?;
    let gcc_install = literal(grants.mull_toolchain()?.gcc_install_dir())?;
    let (language, standard, mode) = if inputs.source.ends_with(".c") {
        ("C", 11, "--driver-mode=gcc")
    } else {
        ("CXX", 17, "--driver-mode=g++")
    };
    Ok(format!(
        "cmake_minimum_required(VERSION 3.28)\n\
         set(CMAKE_MAKE_PROGRAM \"{ninja}\" CACHE FILEPATH \"\" FORCE)\n\
         set(CMAKE_{language}_COMPILER \"{clang}\" CACHE FILEPATH \"\" FORCE)\n\
         set(CMAKE_{language}_COMPILER_ARG1 \"{mode} --no-default-config --gcc-install-dir={gcc_install}\" CACHE STRING \"\" FORCE)\n\
         set(CMAKE_EXE_LINKER_FLAGS \"--ld-path={linker}\" CACHE STRING \"\" FORCE)\n\
         set(CMAKE_EXPORT_NO_PACKAGE_REGISTRY ON)\n\
         set(CMAKE_FIND_USE_PACKAGE_REGISTRY OFF)\n\
         set(CMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY OFF)\n\
         project(forge_mull LANGUAGES {language})\n\
         add_executable(forge-mull-test \"{source}\" \"{driver}\")\n\
         set_target_properties(forge-mull-test PROPERTIES {language}_STANDARD {standard} {language}_STANDARD_REQUIRED YES {language}_EXTENSIONS NO)\n\
         target_compile_options(forge-mull-test PRIVATE -O0 -g -grecord-command-line -fprofile-instr-generate -fcoverage-mapping \"-fpass-plugin={plugin}\")\n\
         target_link_options(forge-mull-test PRIVATE -fprofile-instr-generate \"--ld-path={linker}\")\n"
    ))
}

struct Definition {
    path: PathBuf,
    directory: DirectoryBinding,
    files: Vec<(PathBuf, BoundFile)>,
}
impl Definition {
    fn create(inputs: &Inputs, worktree: &Path, grants: &Grants) -> Result<Self> {
        let cmake = definition(inputs, worktree, grants)?;
        let config = format!(
            "includePaths:\n  - {}\n",
            serde_json::to_string(&format!(
                "^{}$",
                regex::escape(literal(&worktree.join(&inputs.source))?)
            ))?
        );
        let path = grants
            .scratch_parent
            .join(crate::resource_id("mull-definition")?);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let result = (|| -> Result<Self> {
            grants.validate_private_root(&path)?;
            let directory = DirectoryBinding::open(&path, None)?;
            let mut files = Vec::new();
            for (name, text) in [("CMakeLists.txt", cmake), ("mull.yml", config)] {
                let file_path = path.join(name);
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&file_path)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                let (held, observed) = crate::candidate_inputs::repository_live(&file_path, 65536)?;
                ensure!(
                    observed == text.as_bytes(),
                    "Mull generated definition changed"
                );
                files.push((file_path, held));
            }
            let result = Self {
                path: path.clone(),
                directory,
                files,
            };
            result.verify()?;
            Ok(result)
        })();
        result.with_context(|| {
            format!(
                "Mull definition construction failed; retained {}",
                path.display()
            )
        })
    }
    fn verify(&self) -> Result<()> {
        self.directory.verify()?;
        let mut names: Vec<_> = fs::read_dir(&self.path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?;
        names.sort();
        ensure!(
            names == ["CMakeLists.txt", "mull.yml"],
            "Mull definition directory changed"
        );
        for (_, file) in &self.files {
            file.verify()?;
        }
        self.directory.verify()
    }
    fn remove(self) -> Result<()> {
        self.verify()?;
        for (path, _) in &self.files {
            fs::remove_file(path)?;
        }
        self.directory.verify()?;
        fs::remove_dir(&self.path)?;
        ensure!(
            matches!(fs::symlink_metadata(&self.path), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
            "Mull definition remains after removal"
        );
        Ok(())
    }
}

pub(crate) struct State {
    definition: RefCell<Option<Definition>>,
    root: PathBuf,
    executable: RefCell<Option<BoundFile>>,
}
impl State {
    pub(crate) fn create(
        inputs: &Inputs,
        worktree: &Path,
        root: &Path,
        grants: &Grants,
    ) -> Result<Self> {
        let definition = Definition::create(inputs, worktree, grants)?;
        Ok(Self {
            definition: RefCell::new(Some(definition)),
            root: root.to_owned(),
            executable: RefCell::new(None),
        })
    }
    pub(crate) fn read_only(&self) -> Result<PathBuf> {
        Ok(self
            .definition
            .borrow()
            .as_ref()
            .context("Mull definition removed")?
            .path
            .clone())
    }
    pub(crate) fn environment(&self) -> Result<BTreeMap<String, String>> {
        Ok(BTreeMap::from([
            (
                "PATH".into(),
                self.root.join("bin").to_string_lossy().into_owned(),
            ),
            (
                "MULL_CONFIG".into(),
                self.read_only()?
                    .join("mull.yml")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "LLVM_PROFILE_FILE".into(),
                self.root
                    .join("mull-output/baseline.profraw")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("CMAKE_BUILD_PARALLEL_LEVEL".into(), "1".into()),
            ("LANG".into(), "C".into()),
            ("TZ".into(), "UTC".into()),
        ]))
    }
    pub(crate) fn verify(&self) -> Result<()> {
        if let Some(definition) = self.definition.borrow().as_ref() {
            definition.verify()?;
        }
        if let Some(executable) = self.executable.borrow().as_ref() {
            executable.verify()?;
        }
        Ok(())
    }
    pub(crate) fn build(&self) -> PathBuf {
        self.root.join("mull-build")
    }
    pub(crate) fn output(&self) -> PathBuf {
        self.root.join("mull-output")
    }
    pub(crate) fn executable(&self) -> PathBuf {
        self.build().join("forge-mull-test")
    }
    pub(crate) fn admit_executable(&self) -> Result<String> {
        ensure!(
            self.executable.borrow().is_none(),
            "Mull executable already admitted"
        );
        let path = self.executable();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file()
                && meta.mode() & 0o111 != 0
                && meta.mode() & 0o7022 == 0
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1,
            "Mull build did not produce a private regular executable: mode={:o}, uid={}, links={}",
            meta.mode(),
            meta.uid(),
            meta.nlink()
        );
        let (held, bytes) = crate::candidate_inputs::repository_live(&path, 32 * 1024 * 1024)?;
        let hash = crate::mutation_campaign::sha256_hex(&bytes);
        held.verify()?;
        *self.executable.borrow_mut() = Some(held);
        Ok(hash)
    }
    pub(crate) fn programs(
        &self,
        granted: &BTreeMap<String, PathBuf>,
    ) -> Result<BTreeMap<String, PathBuf>> {
        self.verify()?;
        let mut programs = granted.clone();
        if self.executable.borrow().is_some() {
            programs.insert("forge-mull-test".into(), self.executable());
        }
        Ok(programs)
    }
    pub(crate) fn record(&self, receipt: &mut crate::mutation_score::Receipt) -> Result<()> {
        receipt.insert(
            "mull_definition".into(),
            json!({"schema_version":1,"path":self.read_only()?,"read_only":true,"removed":false}),
        );
        Ok(())
    }
    /// Only after the common owner proves all command groups empty and inputs unchanged.
    pub(crate) fn finish(&self) -> Result<()> {
        self.verify()?;
        self.definition
            .borrow_mut()
            .take()
            .context("Mull definition already removed")?
            .remove()
    }
    pub(crate) fn prelaunch_error(&self, error: anyhow::Error) -> anyhow::Error {
        let path = self
            .definition
            .borrow()
            .as_ref()
            .map(|definition| definition.path.clone());
        match self.finish() {
            Ok(()) => error,
            Err(cleanup) => error.context(format!(
                "Mull prelaunch definition cleanup failed; retained {path:?}: {cleanup:#}"
            )),
        }
    }
}
