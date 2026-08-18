use std::path::{Component, Path, PathBuf};

use crate::{WorkflowError, bridge::Agent};

/// Folder name for this specific skill.
const SKILL_NAME: &str = "ren-workflow";

/// The embedded skill entrypoint, following the open Agent Skills standard.
///
/// This is a thin bootstrap: it points agents at the binary, whose `--help` and
/// injected `agent_protocol` are the version-matched source of truth. Rich,
/// version-sensitive guidance lives in [`crate::guide`], not in this file.
pub const SKILL_MD: &str = include_str!("../assets/skill/SKILL.md");

/// UI-facing metadata installed alongside [`SKILL_MD`].
pub const OPENAI_YAML: &str = include_str!("../assets/skill/agents/openai.yaml");

/// A single embedded skill file and its path relative to the skill folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkillFile {
    /// Path relative to the skill folder, using `/` separators.
    pub relative: &'static str,
    /// File contents.
    pub contents: &'static str,
}

/// An embedded skill that can be installed into every supported agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedSkill {
    /// Folder name under the agent's skill root.
    pub name: &'static str,
    /// Files to install relative to the skill folder.
    pub files: &'static [SkillFile],
}

/// Every file that makes up the embedded skill.
pub const SKILL_FILES: &[SkillFile] = &[
    SkillFile {
        relative: "SKILL.md",
        contents: SKILL_MD,
    },
    SkillFile {
        relative: "agents/openai.yaml",
        contents: OPENAI_YAML,
    },
];

/// The embedded `ren-workflow` skill.
pub const WORKFLOW_SKILL: EmbeddedSkill = EmbeddedSkill {
    name: SKILL_NAME,
    files: SKILL_FILES,
};

/// Whether a skill is installed globally (user scope) or in a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitScope {
    /// The agent's user-global config directory (the default).
    User,
    /// The current repository's config directory.
    Project,
}

/// The resolved plan for installing the skill for one agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDefinition {
    /// Filesystem authority beneath which every target must remain.
    pub base_dir: PathBuf,
    /// The `<agent>/skills/<skill-name>` folder that receives the skill.
    pub dir: PathBuf,
    /// Absolute paths and contents of every file to write.
    pub files: Vec<(PathBuf, &'static str)>,
}

/// Returns the agent config directory name (e.g. `.grok`) for `agent`.
const fn agent_config_dir(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => ".claude",
        Agent::Cursor => ".cursor",
        Agent::Codex => ".codex",
        Agent::Grok => ".grok",
        Agent::OpenCode => ".opencode",
        Agent::Pi => ".pi",
    }
}

/// The skill root relative to an agent's config directory for a scope.
///
/// Most agents read `<config>/skills` everywhere. pi keeps user-global skills
/// one level deeper under `<config>/agent/skills`, while project skills stay
/// at `<config>/skills`.
const fn agent_skill_root(
    scope: InitScope,
    agent: Agent,
) -> &'static str {
    match (agent, scope) {
        (Agent::Pi, InitScope::User) => "agent/skills",
        _ => "skills",
    }
}

/// Every agent the skill installer supports.
#[must_use]
pub const fn supported_agents() -> [Agent; 6] {
    [
        Agent::Claude,
        Agent::Cursor,
        Agent::Codex,
        Agent::Grok,
        Agent::OpenCode,
        Agent::Pi,
    ]
}

/// Builds the install plan for one agent rooted at `base_dir`.
///
/// `base_dir` is the user's home directory for [`InitScope::User`] or the
/// repository root for [`InitScope::Project`]; both resolve to
/// `<agent>/skills/ren-workflow` for every agent, except pi, whose user-global
/// skills live at `<agent>/agent/skills/ren-workflow`.
#[must_use]
pub fn skill_definition(
    base_dir: &Path,
    scope: InitScope,
    agent: Agent,
) -> SkillDefinition {
    skill_definition_for(base_dir, scope, agent, WORKFLOW_SKILL)
}

/// Builds the install plan for any embedded `skill` rooted at `base_dir`.
#[must_use]
pub fn skill_definition_for(
    base_dir: &Path,
    scope: InitScope,
    agent: Agent,
    skill: EmbeddedSkill,
) -> SkillDefinition {
    let dir = base_dir
        .join(agent_config_dir(agent))
        .join(agent_skill_root(scope, agent))
        .join(skill.name);
    let files = skill
        .files
        .iter()
        .map(|file| (join_relative(&dir, file.relative), file.contents))
        .collect();
    SkillDefinition {
        base_dir: base_dir.to_path_buf(),
        dir,
        files,
    }
}

/// Joins a `/`-separated relative path onto `base` in a platform-correct way.
fn join_relative(
    base: &Path,
    relative: &str,
) -> PathBuf {
    let mut path = base.to_path_buf();
    for segment in relative.split('/') {
        path.push(segment);
    }
    path
}

/// Writes every file in `definition`, creating parent directories as needed.
///
/// Existing byte-identical files are left unchanged, making repeated installs
/// idempotent. All files are checked before the first write so a later conflict
/// cannot leave a partially installed skill.
///
/// # Errors
///
/// Returns [`WorkflowError::SkillExists`] when a target file has different
/// contents and `force` is false, [`WorkflowError::UnsafeSkillPath`] when a
/// target traverses a symbolic link or escapes its base directory, or
/// [`WorkflowError::Io`] when a filesystem operation fails.
pub fn install_skill(
    definition: &SkillDefinition,
    force: bool,
) -> Result<(), WorkflowError> {
    install_skills(std::slice::from_ref(definition), force)
}

/// Writes every file in `definitions` after preflighting the complete batch.
///
/// This is used by top-level initialization so conflicts in later agents,
/// skills, or files are reported before any earlier target is written.
///
/// # Errors
///
/// Returns the same installation errors as [`install_skill`].
pub fn install_skills(
    definitions: &[SkillDefinition],
    force: bool,
) -> Result<(), WorkflowError> {
    install_skills_inner(definitions, force, || {})
}

#[cfg(test)]
pub fn install_skills_with_pre_apply_hook(
    definitions: &[SkillDefinition],
    force: bool,
    before_apply: impl FnOnce(),
) -> Result<(), WorkflowError> {
    install_skills_inner(definitions, force, before_apply)
}

fn install_skills_inner(
    definitions: &[SkillDefinition],
    force: bool,
    before_apply: impl FnOnce(),
) -> Result<(), WorkflowError> {
    validate_definitions(definitions)?;
    platform::install(definitions, force, before_apply)
}

fn validate_definitions(definitions: &[SkillDefinition]) -> Result<(), WorkflowError> {
    for definition in definitions {
        if definition.base_dir.as_os_str().is_empty()
            || !is_safe_relative(
                definition
                    .dir
                    .strip_prefix(&definition.base_dir)
                    .map_err(|_| WorkflowError::UnsafeSkillPath(definition.dir.clone()))?,
            )
        {
            return Err(WorkflowError::UnsafeSkillPath(definition.dir.clone()));
        }
        for (path, _) in &definition.files {
            let relative = path
                .strip_prefix(&definition.base_dir)
                .map_err(|_| WorkflowError::UnsafeSkillPath(path.clone()))?;
            if !path.starts_with(&definition.dir) || !is_safe_relative(relative) {
                return Err(WorkflowError::UnsafeSkillPath(path.clone()));
            }
        }
    }
    Ok(())
}

fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(unix)]
mod platform {
    use std::{
        ffi::{OsStr, OsString},
        fs::{self, File, Metadata},
        io::{self, Read as _, Seek as _, SeekFrom, Write as _},
        os::unix::fs::MetadataExt as _,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use rustix::{
        fs::{AtFlags, Mode, OFlags, mkdirat, open, openat, renameat, unlinkat},
        io::Errno,
    };

    use super::{SkillDefinition, WorkflowError};

    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const READ_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);
    const CREATE_FLAGS: OFlags = OFlags::WRONLY
        .union(OFlags::CREATE)
        .union(OFlags::EXCL)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    /// Existing skill files are tiny text assets. Bounding rollback capture
    /// prevents a hostile sparse or device-like target from consuming memory.
    const MAX_EXISTING_SKILL_BYTES: u64 = 1024 * 1024;
    const TEMP_ATTEMPTS: u64 = 128;
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    pub(super) fn install(
        definitions: &[SkillDefinition],
        force: bool,
        before_apply: impl FnOnce(),
    ) -> Result<(), WorkflowError> {
        let mut created_dirs = Vec::new();
        let pending = match preflight(definitions, force, &mut created_dirs) {
            Ok(pending) => pending,
            Err(error) => {
                return Err(with_rollback_error(
                    error,
                    cleanup_directories(&created_dirs),
                ));
            },
        };

        before_apply();
        apply(pending, &created_dirs)
    }

    #[derive(Debug)]
    struct RootHandle {
        path: PathBuf,
        dir: File,
        identity: Identity,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
    }

    #[derive(Debug)]
    struct PendingSkillFile {
        root: Arc<RootHandle>,
        parent: File,
        parent_relative: PathBuf,
        name: OsString,
        path: PathBuf,
        contents: &'static str,
        previous: Option<Vec<u8>>,
        target: Option<File>,
    }

    #[derive(Debug)]
    struct CreatedDirectory {
        parent: File,
        name: OsString,
        path: PathBuf,
        identity: Identity,
    }

    fn preflight(
        definitions: &[SkillDefinition],
        force: bool,
        created_dirs: &mut Vec<CreatedDirectory>,
    ) -> Result<Vec<PendingSkillFile>, WorkflowError> {
        let mut pending = Vec::new();
        for definition in definitions {
            let root = Arc::new(open_root(&definition.base_dir)?);
            for (path, contents) in &definition.files {
                let relative = path
                    .strip_prefix(&definition.base_dir)
                    .map_err(|_| WorkflowError::UnsafeSkillPath(path.clone()))?;
                let parent_relative = relative
                    .parent()
                    .ok_or_else(|| WorkflowError::UnsafeSkillPath(path.clone()))?;
                let name = relative
                    .file_name()
                    .ok_or_else(|| WorkflowError::UnsafeSkillPath(path.clone()))?
                    .to_os_string();
                let parent = open_or_create_directories(
                    &root.dir,
                    &root.path,
                    parent_relative,
                    created_dirs,
                )?;
                match read_target(&parent, &name, path)? {
                    Some((installed, read_target)) if installed == contents.as_bytes() => {
                        drop(read_target);
                    },
                    Some((_, _)) if !force => {
                        return Err(WorkflowError::SkillExists(path.clone()));
                    },
                    Some((installed, read_target)) => {
                        pending.push(PendingSkillFile {
                            root: Arc::clone(&root),
                            parent,
                            parent_relative: parent_relative.to_path_buf(),
                            name,
                            path: path.clone(),
                            contents,
                            previous: Some(installed),
                            target: Some(read_target),
                        });
                    },
                    None => pending.push(PendingSkillFile {
                        root: Arc::clone(&root),
                        parent,
                        parent_relative: parent_relative.to_path_buf(),
                        name,
                        path: path.clone(),
                        contents,
                        previous: None,
                        target: None,
                    }),
                }
            }
        }
        Ok(pending)
    }

    fn open_root(path: &Path) -> Result<RootHandle, WorkflowError> {
        let canonical = fs::canonicalize(path).map_err(|error| WorkflowError::io(path, error))?;
        let dir = open_absolute_directory(&canonical)?;
        let metadata = dir
            .metadata()
            .map_err(|error| WorkflowError::io(&canonical, error))?;
        if !metadata.is_dir() {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        Ok(RootHandle {
            path: canonical,
            identity: identity(&metadata),
            dir,
        })
    }

    /// Opens an absolute directory one component at a time from a stable `/`
    /// descriptor. A canonical path is used so legitimate system aliases such
    /// as macOS `/var` remain supported, while every traversed component is
    /// still opened with `O_NOFOLLOW`.
    fn open_absolute_directory(path: &Path) -> Result<File, WorkflowError> {
        if !path.is_absolute() {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        let root = open(Path::new("/"), DIRECTORY_FLAGS, Mode::empty())
            .map(File::from)
            .map_err(|error| component_error(error, Path::new("/")))?;
        let mut current = root;
        let mut display = PathBuf::from("/");
        for component in path.components() {
            match component {
                std::path::Component::RootDir => {},
                std::path::Component::Normal(name) => {
                    display.push(name);
                    current = openat(&current, name, DIRECTORY_FLAGS, Mode::empty())
                        .map(File::from)
                        .map_err(|error| component_error(error, &display))?;
                },
                _ => return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf())),
            }
        }
        Ok(current)
    }

    fn open_or_create_directories(
        root: &File,
        root_path: &Path,
        relative: &Path,
        created_dirs: &mut Vec<CreatedDirectory>,
    ) -> Result<File, WorkflowError> {
        let mut current = root
            .try_clone()
            .map_err(|error| WorkflowError::io(root_path, error))?;
        let mut display = root_path.to_path_buf();
        for component in relative.components() {
            let name = component.as_os_str();
            display.push(name);
            match openat(&current, name, DIRECTORY_FLAGS, Mode::empty()) {
                Ok(fd) => current = File::from(fd),
                Err(Errno::NOENT) => {
                    let created = match mkdirat(&current, name, Mode::from_raw_mode(0o755)) {
                        Ok(()) => true,
                        Err(Errno::EXIST) => false,
                        Err(error) => return Err(WorkflowError::io(&display, error.into())),
                    };
                    let child = openat(&current, name, DIRECTORY_FLAGS, Mode::empty())
                        .map(File::from)
                        .map_err(|error| component_error(error, &display))?;
                    let metadata = child
                        .metadata()
                        .map_err(|error| WorkflowError::io(&display, error))?;
                    if !metadata.is_dir() {
                        return Err(WorkflowError::UnsafeSkillPath(display));
                    }
                    if created {
                        created_dirs.push(CreatedDirectory {
                            parent: current
                                .try_clone()
                                .map_err(|error| WorkflowError::io(&display, error))?,
                            name: name.to_os_string(),
                            path: display.clone(),
                            identity: identity(&metadata),
                        });
                    }
                    current = child;
                },
                Err(error) => return Err(component_error(error, &display)),
            }
        }
        Ok(current)
    }

    fn read_target(
        parent: &File,
        name: &OsStr,
        path: &Path,
    ) -> Result<Option<(Vec<u8>, File)>, WorkflowError> {
        let mut target = match openat(parent, name, READ_FLAGS, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(component_error(error, path)),
        };
        let metadata = target
            .metadata()
            .map_err(|error| WorkflowError::io(path, error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        if metadata.len() > MAX_EXISTING_SKILL_BYTES {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        let mut installed = Vec::new();
        std::io::Read::by_ref(&mut target)
            .take(MAX_EXISTING_SKILL_BYTES + 1)
            .read_to_end(&mut installed)
            .map_err(|error| WorkflowError::io(path, error))?;
        if u64::try_from(installed.len()).unwrap_or(u64::MAX) > MAX_EXISTING_SKILL_BYTES {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        Ok(Some((installed, target)))
    }

    fn open_existing_target(
        parent: &File,
        name: &OsStr,
        path: &Path,
        flags: OFlags,
    ) -> Result<File, WorkflowError> {
        let target = openat(parent, name, flags, Mode::empty())
            .map(File::from)
            .map_err(|error| component_error(error, path))?;
        let metadata = target
            .metadata()
            .map_err(|error| WorkflowError::io(path, error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(WorkflowError::UnsafeSkillPath(path.to_path_buf()));
        }
        Ok(target)
    }

    fn apply(
        mut pending: Vec<PendingSkillFile>,
        created_dirs: &[CreatedDirectory],
    ) -> Result<(), WorkflowError> {
        let mut completed = Vec::new();
        for index in 0..pending.len() {
            if let Err(error) = verify_pending_path(&pending[index]) {
                return Err(rollback_after(
                    error,
                    &mut pending,
                    &completed,
                    created_dirs,
                ));
            }

            if pending[index].previous.is_some() {
                let (temporary_name, replacement) = match create_replacement(&pending[index]) {
                    Ok(replacement) => replacement,
                    Err(error) => {
                        return Err(rollback_after(
                            error,
                            &mut pending,
                            &completed,
                            created_dirs,
                        ));
                    },
                };
                if let Err(error) = verify_pending_path(&pending[index]) {
                    let cleanup_error =
                        unlinkat(&pending[index].parent, &temporary_name, AtFlags::empty())
                            .err()
                            .map(io::Error::from);
                    return Err(rollback_after(
                        with_rollback_error(error, cleanup_error),
                        &mut pending,
                        &completed,
                        created_dirs,
                    ));
                }
                if let Err(error) = renameat(
                    &pending[index].parent,
                    &temporary_name,
                    &pending[index].parent,
                    &pending[index].name,
                ) {
                    let cleanup_error =
                        unlinkat(&pending[index].parent, &temporary_name, AtFlags::empty())
                            .err()
                            .map(io::Error::from);
                    return Err(rollback_after(
                        with_rollback_error(
                            component_error(error, &pending[index].path),
                            cleanup_error,
                        ),
                        &mut pending,
                        &completed,
                        created_dirs,
                    ));
                }
                pending[index].target = Some(replacement);
            } else {
                let target = match openat(
                    &pending[index].parent,
                    &pending[index].name,
                    CREATE_FLAGS,
                    Mode::from_raw_mode(0o644),
                ) {
                    Ok(fd) => File::from(fd),
                    Err(error) => {
                        return Err(rollback_after(
                            component_error(error, &pending[index].path),
                            &mut pending,
                            &completed,
                            created_dirs,
                        ));
                    },
                };
                pending[index].target = Some(target);
                if let Err(error) = write_contents(&mut pending[index]) {
                    completed.push(index);
                    return Err(rollback_after(
                        error,
                        &mut pending,
                        &completed,
                        created_dirs,
                    ));
                }
            }
            completed.push(index);
            if let Err(error) = verify_pending_path(&pending[index]) {
                return Err(rollback_after(
                    error,
                    &mut pending,
                    &completed,
                    created_dirs,
                ));
            }
        }
        Ok(())
    }

    fn create_replacement(pending: &PendingSkillFile) -> Result<(OsString, File), WorkflowError> {
        for _ in 0..TEMP_ATTEMPTS {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = OsString::from(format!(
                ".ren-skill-install-{}-{sequence}",
                std::process::id()
            ));
            let mut target = match openat(
                &pending.parent,
                &name,
                CREATE_FLAGS,
                Mode::from_raw_mode(0o644),
            ) {
                Ok(fd) => File::from(fd),
                Err(Errno::EXIST) => continue,
                Err(error) => return Err(component_error(error, &pending.path)),
            };
            if let Err(error) = target.write_all(pending.contents.as_bytes()) {
                let cleanup_error = unlinkat(&pending.parent, &name, AtFlags::empty())
                    .err()
                    .map(io::Error::from);
                return Err(with_rollback_error(
                    WorkflowError::io(&pending.path, error),
                    cleanup_error,
                ));
            }
            return Ok((name, target));
        }
        Err(WorkflowError::io(
            &pending.path,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "could not allocate a private staging file beside {}",
                    pending.path.display()
                ),
            ),
        ))
    }

    fn write_contents(file: &mut PendingSkillFile) -> Result<(), WorkflowError> {
        let target = file
            .target
            .as_mut()
            .ok_or_else(|| WorkflowError::UnsafeSkillPath(file.path.clone()))?;
        target
            .set_len(0)
            .map_err(|error| WorkflowError::io(&file.path, error))?;
        target
            .seek(SeekFrom::Start(0))
            .map_err(|error| WorkflowError::io(&file.path, error))?;
        target
            .write_all(file.contents.as_bytes())
            .map_err(|error| WorkflowError::io(&file.path, error))?;
        Ok(())
    }

    fn verify_pending_path(file: &PendingSkillFile) -> Result<(), WorkflowError> {
        verify_root(&file.root)?;
        let current_parent =
            open_existing_directories(&file.root.dir, &file.root.path, &file.parent_relative)?;
        if identity(
            &current_parent
                .metadata()
                .map_err(|error| WorkflowError::io(&file.parent_relative, error))?,
        ) != identity(
            &file
                .parent
                .metadata()
                .map_err(|error| WorkflowError::io(&file.path, error))?,
        ) {
            return Err(WorkflowError::UnsafeSkillPath(file.path.clone()));
        }
        if let Some(target) = &file.target {
            let current =
                open_existing_target(&current_parent, &file.name, &file.path, READ_FLAGS)?;
            if identity(
                &current
                    .metadata()
                    .map_err(|error| WorkflowError::io(&file.path, error))?,
            ) != identity(
                &target
                    .metadata()
                    .map_err(|error| WorkflowError::io(&file.path, error))?,
            ) {
                return Err(WorkflowError::UnsafeSkillPath(file.path.clone()));
            }
        }
        Ok(())
    }

    fn verify_root(root: &RootHandle) -> Result<(), WorkflowError> {
        let current = open_absolute_directory(&root.path)?;
        if identity(
            &current
                .metadata()
                .map_err(|error| WorkflowError::io(&root.path, error))?,
        ) != root.identity
        {
            return Err(WorkflowError::UnsafeSkillPath(root.path.clone()));
        }
        Ok(())
    }

    fn open_existing_directories(
        root: &File,
        root_path: &Path,
        relative: &Path,
    ) -> Result<File, WorkflowError> {
        let mut current = root
            .try_clone()
            .map_err(|error| WorkflowError::io(root_path, error))?;
        let mut display = root_path.to_path_buf();
        for component in relative.components() {
            display.push(component.as_os_str());
            current = openat(
                &current,
                component.as_os_str(),
                DIRECTORY_FLAGS,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|error| component_error(error, &display))?;
        }
        Ok(current)
    }

    fn rollback_after(
        install_error: WorkflowError,
        pending: &mut [PendingSkillFile],
        completed: &[usize],
        created_dirs: &[CreatedDirectory],
    ) -> WorkflowError {
        let mut rollback_error = None;
        for index in completed.iter().rev().copied() {
            let file = &mut pending[index];
            let result = match &file.previous {
                Some(previous) => restore_existing(file.target.as_mut(), previous),
                None => remove_created_file(file),
            };
            if let Err(error) = result
                && rollback_error.is_none()
            {
                rollback_error = Some(error);
            }
        }
        if let Some(error) = cleanup_directories(created_dirs)
            && rollback_error.is_none()
        {
            rollback_error = Some(error);
        }
        with_rollback_error(install_error, rollback_error)
    }

    fn restore_existing(
        target: Option<&mut File>,
        previous: &[u8],
    ) -> io::Result<()> {
        let target = target.ok_or_else(|| io::Error::other("missing rollback file handle"))?;
        target.set_len(0)?;
        target.seek(SeekFrom::Start(0))?;
        target.write_all(previous)
    }

    fn remove_created_file(file: &PendingSkillFile) -> io::Result<()> {
        let Some(target) = &file.target else {
            return Ok(());
        };
        let current = match openat(&file.parent, &file.name, READ_FLAGS, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(Errno::NOENT) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if identity(&current.metadata()?) != identity(&target.metadata()?) {
            return Err(io::Error::other(format!(
                "refusing to remove a replaced skill target at {}",
                file.path.display()
            )));
        }
        unlinkat(&file.parent, &file.name, AtFlags::empty()).map_err(Into::into)
    }

    fn cleanup_directories(created_dirs: &[CreatedDirectory]) -> Option<io::Error> {
        let mut first_error = None;
        for directory in created_dirs.iter().rev() {
            let current = match openat(
                &directory.parent,
                &directory.name,
                DIRECTORY_FLAGS,
                Mode::empty(),
            ) {
                Ok(fd) => File::from(fd),
                Err(Errno::NOENT) => continue,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error.into());
                    }
                    continue;
                },
            };
            let metadata = match current.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    continue;
                },
            };
            if identity(&metadata) != directory.identity {
                if first_error.is_none() {
                    first_error = Some(io::Error::other(format!(
                        "refusing to remove a replaced skill directory at {}",
                        directory.path.display()
                    )));
                }
                continue;
            }
            if let Err(error) = unlinkat(&directory.parent, &directory.name, AtFlags::REMOVEDIR)
                && error != Errno::NOTEMPTY
                && first_error.is_none()
            {
                first_error = Some(error.into());
            }
        }
        first_error
    }

    fn component_error(
        error: Errno,
        path: &Path,
    ) -> WorkflowError {
        if matches!(error, Errno::LOOP | Errno::NOTDIR) {
            WorkflowError::UnsafeSkillPath(path.to_path_buf())
        } else {
            WorkflowError::io(path, error.into())
        }
    }

    fn identity(metadata: &Metadata) -> Identity {
        Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn with_rollback_error(
        install_error: WorkflowError,
        rollback_error: Option<io::Error>,
    ) -> WorkflowError {
        match rollback_error {
            None => install_error,
            Some(rollback_error) => WorkflowError::io(
                ".",
                io::Error::other(format!(
                    "skill installation failed: {install_error}; rollback also failed: \
                     {rollback_error}"
                )),
            ),
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use super::{SkillDefinition, WorkflowError};

    pub(super) fn install(
        definitions: &[SkillDefinition],
        _force: bool,
        _before_apply: impl FnOnce(),
    ) -> Result<(), WorkflowError> {
        let rejected = definitions
            .iter()
            .flat_map(|definition| definition.files.iter())
            .map(|(path, _)| path.clone())
            .next()
            .or_else(|| {
                definitions
                    .first()
                    .map(|definition| definition.base_dir.clone())
            })
            .unwrap_or_default();
        Err(WorkflowError::UnsafeSkillPath(rejected))
    }
}
