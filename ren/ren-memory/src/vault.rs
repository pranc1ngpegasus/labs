use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::{MemoryError, Result},
    fsutil::{create_private_dir, open_private_lock, write_atomic_replace},
};

const REGISTRY_SCHEMA: &str = "ren-memory-registry/v1";
const CONFIG_SCHEMA: &str = "ren-memory-config/v1";
const PROMOTION_WORKFLOW: &str = include_str!("../bundled/zettelkasten-promote.rhai");
const MAX_GIT_POINTER_BYTES: u64 = 4096;

#[derive(Clone, Debug, Deserialize)]
struct MemoryConfig {
    schema: String,
    #[serde(default = "default_true")]
    redact_secrets: bool,
    #[serde(default)]
    hooks: HookPolicy,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HookPolicy {
    #[serde(default)]
    auto_register_unmatched: bool,
    #[serde(default)]
    allow_paths: Vec<PathBuf>,
    #[serde(default)]
    deny_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct GitRepository {
    common_dir: PathBuf,
    primary_worktree: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VaultEntry {
    pub root: PathBuf,
    pub project_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Registry {
    schema: String,
    #[serde(default)]
    pub vaults: BTreeMap<String, VaultEntry>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema: REGISTRY_SCHEMA.into(),
            vaults: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryHome {
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Vault {
    pub id: String,
    pub root: PathBuf,
    pub project_path: PathBuf,
    pub index_root: PathBuf,
}

pub struct WriterLock {
    file: File,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

impl MemoryHome {
    /// Resolves the user-scoped memory home.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::HomeUnavailable`] when neither
    /// `REN_MEMORY_HOME` nor `HOME` is available.
    pub fn discover() -> Result<Self> {
        if let Some(path) = env::var_os("REN_MEMORY_HOME") {
            return Ok(Self {
                root: PathBuf::from(path),
            });
        }
        let home = env::var_os("HOME").ok_or(MemoryError::HomeUnavailable)?;
        Ok(Self {
            root: PathBuf::from(home).join(".ren").join("memory"),
        })
    }

    /// Creates the private user-scope layout and default metadata files.
    ///
    /// # Errors
    ///
    /// Returns a filesystem or serialization error when the layout cannot be
    /// initialized.
    pub fn initialize(&self) -> Result<()> {
        create_private_dir(&self.root)?;
        create_private_dir(&self.root.join("vaults"))?;
        create_private_dir(&self.root.join("indexes"))?;
        let registry_lock = open_private_lock(&self.root.join("registry.lock"))?;
        registry_lock
            .lock_exclusive()
            .map_err(|error| MemoryError::io(self.root.join("registry.lock"), error))?;
        let registry = self.registry_path();
        if !registry.exists() {
            self.save_registry(&Registry::default())?;
        }
        let config = self.root.join("config.toml");
        if !config.exists() {
            write_atomic_replace(
                &config,
                b"schema = \"ren-memory-config/v1\"\nredact_secrets = true\n\n[hooks]\n\
                  auto_register_unmatched = false\nallow_paths = []\ndeny_paths = []\n",
            )?;
        }
        let workflow_directory = self.root.parent().unwrap_or(&self.root).join("workflows");
        create_private_dir(&workflow_directory)?;
        let promotion_workflow = workflow_directory.join("zettelkasten-promote.rhai");
        if !promotion_workflow.exists() {
            write_atomic_replace(&promotion_workflow, PROMOTION_WORKFLOW.as_bytes())?;
        }
        Ok(())
    }

    /// Reads and validates the vault registry.
    ///
    /// # Errors
    ///
    /// Returns a filesystem, JSON, or unsupported-schema error for an invalid
    /// registry.
    pub fn load_registry(&self) -> Result<Registry> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Registry::default());
        }
        let bytes = fs::read(&path).map_err(|error| MemoryError::io(&path, error))?;
        let registry: Registry = serde_json::from_slice(&bytes)?;
        if registry.schema != REGISTRY_SCHEMA {
            return Err(MemoryError::InvalidConfig(format!(
                "unsupported registry schema `{}`",
                registry.schema
            )));
        }
        Ok(registry)
    }

    /// Registers a project with a managed Markdown vault.
    ///
    /// # Errors
    ///
    /// Returns a configuration or filesystem error for invalid identifiers,
    /// conflicting registrations, or inaccessible directories.
    pub fn register(
        &self,
        requested_id: Option<&str>,
        requested_root: Option<&Path>,
        project_path: &Path,
    ) -> Result<Vault> {
        self.initialize()?;
        let project_path = canonical_directory(project_path)?;
        let id = requested_id.map_or_else(|| default_id(&project_path), str::to_owned);
        validate_vault_id(&id)?;
        let requested_root =
            requested_root.map_or_else(|| self.root.join("vaults").join(&id), Path::to_path_buf);

        let registry_path = self.root.join("registry.lock");
        let registry_lock = open_private_lock(&registry_path)?;
        registry_lock
            .lock_exclusive()
            .map_err(|error| MemoryError::io(&registry_path, error))?;
        let mut registry = self.load_registry()?;
        if let Some(existing) = registry.vaults.get(&id) {
            let existing_root = fs::canonicalize(&existing.root)
                .map_err(|error| MemoryError::io(&existing.root, error))?;
            let requested_comparison = canonicalize_existing_or_absolute(&requested_root)?;
            if existing_root != requested_comparison || existing.project_path != project_path {
                return Err(MemoryError::InvalidConfig(format!(
                    "vault id `{id}` is already registered for {}",
                    existing.root.display()
                )));
            }
            return vault_from_entry(&id, existing);
        }
        let requested_comparison = canonicalize_existing_or_absolute(&requested_root)?;
        if let Some((other_id, _)) = registry.vaults.iter().find(|(_, entry)| {
            canonicalize_existing_or_absolute(&entry.root)
                .is_ok_and(|root| root == requested_comparison)
                || entry.project_path == project_path
        }) {
            return Err(MemoryError::InvalidConfig(format!(
                "vault root or project is already registered as `{other_id}`"
            )));
        }
        create_vault_layout(&requested_root)?;
        let root = fs::canonicalize(&requested_root)
            .map_err(|error| MemoryError::io(&requested_root, error))?;
        let index_root = self.root.join("indexes").join(&id);
        create_index_layout(&index_root)?;
        let index_root =
            fs::canonicalize(&index_root).map_err(|error| MemoryError::io(&index_root, error))?;
        registry.vaults.insert(
            id.clone(),
            VaultEntry {
                root: root.clone(),
                project_path: project_path.clone(),
                index_root: Some(index_root.clone()),
            },
        );
        self.save_registry(&registry)?;
        Ok(Vault {
            id,
            root,
            project_path,
            index_root,
        })
    }

    /// Resolves a vault by explicit ID or current-directory association.
    ///
    /// # Errors
    ///
    /// Returns a vault-selection or filesystem error when no unique,
    /// accessible vault can be selected.
    pub fn resolve(
        &self,
        requested_id: Option<&str>,
        cwd: &Path,
    ) -> Result<Vault> {
        let registry = self.load_registry()?;
        if let Some(id) = requested_id {
            let entry = registry
                .vaults
                .get(id)
                .ok_or_else(|| MemoryError::UnknownVault(id.into()))?;
            return vault_from_entry(id, entry);
        }
        let canonical_cwd = canonical_directory(cwd)?;
        let mut matches = registry
            .vaults
            .iter()
            .filter(|(_, entry)| canonical_cwd.starts_with(&entry.project_path))
            .collect::<Vec<_>>();
        matches
            .sort_by_key(|(_, entry)| std::cmp::Reverse(entry.project_path.components().count()));
        if let Some((id, entry)) = matches.first() {
            return vault_from_entry(id, entry);
        }
        if let Some(vault) = resolve_git_worktree(&registry, &canonical_cwd)? {
            return Ok(vault);
        }
        if registry.vaults.len() == 1 {
            let (id, entry) = registry
                .vaults
                .first_key_value()
                .ok_or(MemoryError::VaultNotFound)?;
            return vault_from_entry(id, entry);
        }
        if registry.vaults.is_empty() {
            Err(MemoryError::VaultNotFound)
        } else {
            Err(MemoryError::AmbiguousVault)
        }
    }

    /// Resolves the vault for a hook path, registering it when necessary.
    ///
    /// # Errors
    ///
    /// Returns a registration, configuration, or filesystem error when the
    /// hint cannot be used as a project directory.
    pub fn resolve_or_register_hint(
        &self,
        hint: &Path,
    ) -> Result<Vault> {
        let canonical_hint = canonical_directory(hint)?;
        let config = self.load_config()?;
        if !config.redact_secrets {
            return Err(MemoryError::InvalidConfig(
                "hook capture requires redact_secrets = true".into(),
            ));
        }
        if path_matches_any(&canonical_hint, &config.hooks.deny_paths)? {
            return Err(MemoryError::UnsafeInput(format!(
                "hook capture is denied for {}",
                canonical_hint.display()
            )));
        }
        match self.resolve_strict(&canonical_hint) {
            Ok(vault) => return Ok(vault),
            Err(MemoryError::VaultNotFound) => {},
            Err(error) => return Err(error),
        }
        if config.hooks.auto_register_unmatched
            && !config.hooks.allow_paths.is_empty()
            && path_matches_any(&canonical_hint, &config.hooks.allow_paths)?
        {
            return self.register(None, None, &canonical_hint);
        }
        Err(MemoryError::VaultNotFound)
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    fn save_registry(
        &self,
        registry: &Registry,
    ) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(registry)?;
        write_atomic_replace(&self.registry_path(), &bytes)
    }

    fn resolve_strict(
        &self,
        cwd: &Path,
    ) -> Result<Vault> {
        let registry = self.load_registry()?;
        let canonical_cwd = canonical_directory(cwd)?;
        let mut matches = registry
            .vaults
            .iter()
            .filter(|(_, entry)| canonical_cwd.starts_with(&entry.project_path))
            .collect::<Vec<_>>();
        matches
            .sort_by_key(|(_, entry)| std::cmp::Reverse(entry.project_path.components().count()));
        if let Some((id, entry)) = matches.first() {
            return vault_from_entry(id, entry);
        }
        resolve_git_worktree(&registry, &canonical_cwd)?.ok_or(MemoryError::VaultNotFound)
    }

    fn load_config(&self) -> Result<MemoryConfig> {
        let path = self.root.join("config.toml");
        let input = fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
        let config: MemoryConfig = toml_edit::de::from_str(&input).map_err(|error| {
            MemoryError::InvalidConfig(format!("cannot parse {}: {error}", path.display()))
        })?;
        if config.schema != CONFIG_SCHEMA {
            return Err(MemoryError::InvalidConfig(format!(
                "unsupported memory config schema `{}`",
                config.schema
            )));
        }
        Ok(config)
    }
}

impl Vault {
    #[must_use]
    pub fn index_dir(&self) -> PathBuf {
        self.index_root.clone()
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.index_dir().join("memory.db")
    }

    /// Acquires the per-vault exclusive writer lock.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::WriterBusy`] for a failed nonblocking attempt, or
    /// an I/O error when the lock cannot be opened.
    pub fn lock_writer(
        &self,
        blocking: bool,
    ) -> Result<WriterLock> {
        let path = self.index_dir().join("writer.lock");
        let file = open_private_lock(&path)?;
        let lock_result = if blocking {
            file.lock_exclusive()
        } else {
            file.try_lock_exclusive()
        };
        lock_result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                MemoryError::WriterBusy
            } else {
                MemoryError::io(path, error)
            }
        })?;
        Ok(WriterLock { file })
    }

    /// Builds a managed note path that cannot traverse outside this vault.
    ///
    /// # Errors
    ///
    /// Returns an unsafe-input or filesystem error for invalid directories,
    /// symlink escapes, or inaccessible paths.
    pub fn safe_note_path(
        &self,
        directory: &str,
        id: &str,
    ) -> Result<PathBuf> {
        if !matches!(
            directory,
            "fleeting" | "literature" | "permanent" | "structure" | "index" | "archived"
        ) {
            return Err(MemoryError::UnsafeInput(format!(
                "invalid note directory `{directory}`"
            )));
        }
        let candidate = self.root.join(directory).join(format!("{id}.md"));
        let parent = candidate
            .parent()
            .ok_or_else(|| MemoryError::UnsafeInput("note path has no parent".into()))?;
        let canonical_parent =
            fs::canonicalize(parent).map_err(|error| MemoryError::io(parent, error))?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(MemoryError::UnsafeInput(
                "note path escapes the registered vault".into(),
            ));
        }
        Ok(candidate)
    }
}

fn create_vault_layout(root: &Path) -> Result<()> {
    create_private_dir(root)?;
    for directory in [
        "fleeting",
        "literature",
        "permanent",
        "structure",
        "index",
        "archived",
        ".index",
        ".index/diagnostics",
        ".index/capture-spool",
        ".index/transactions",
        ".revisions",
    ] {
        create_private_dir(&root.join(directory))?;
    }
    Ok(())
}

fn create_index_layout(root: &Path) -> Result<()> {
    create_private_dir(root)?;
    for directory in [
        "diagnostics",
        "capture-spool",
        "capture-events",
        "transactions",
    ] {
        create_private_dir(&root.join(directory))?;
    }
    Ok(())
}

fn vault_from_entry(
    id: &str,
    entry: &VaultEntry,
) -> Result<Vault> {
    let root =
        fs::canonicalize(&entry.root).map_err(|error| MemoryError::io(&entry.root, error))?;
    let project_path = fs::canonicalize(&entry.project_path)
        .map_err(|error| MemoryError::io(&entry.project_path, error))?;
    let index_root = match &entry.index_root {
        Some(index_root) => canonicalize_existing_or_absolute(index_root)?,
        None => root.join(".index"),
    };
    Ok(Vault {
        id: id.into(),
        root,
        project_path,
        index_root,
    })
}

fn canonicalize_existing_or_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()
            .map_err(|error| MemoryError::io(".", error))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {},
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(MemoryError::UnsafeInput(format!(
                        "path escapes its filesystem root: {}",
                        path.display()
                    )));
                }
            },
            Component::Normal(part) => normalized.push(part),
        }
    }
    let mut ancestor = normalized.clone();
    let mut missing = Vec::new();
    while !ancestor
        .try_exists()
        .map_err(|error| MemoryError::io(&ancestor, error))?
    {
        let part = ancestor
            .file_name()
            .ok_or_else(|| MemoryError::InvalidConfig("path has no existing ancestor".into()))?
            .to_owned();
        missing.push(part);
        if !ancestor.pop() {
            return Err(MemoryError::InvalidConfig(
                "path has no existing ancestor".into(),
            ));
        }
    }
    let mut canonical =
        fs::canonicalize(&ancestor).map_err(|error| MemoryError::io(&ancestor, error))?;
    for part in missing.into_iter().rev() {
        canonical.push(part);
    }
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|error| MemoryError::io(path, error))?;
    if !canonical.is_dir() {
        return Err(MemoryError::InvalidConfig(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    Ok(canonical)
}

fn resolve_git_worktree(
    registry: &Registry,
    cwd: &Path,
) -> Result<Option<Vault>> {
    let Some(repository) = git_repository(cwd) else {
        return Ok(None);
    };
    let mut matches = registry
        .vaults
        .iter()
        .filter_map(|(id, entry)| {
            let candidate = git_repository(&entry.project_path)?;
            (candidate.common_dir == repository.common_dir).then_some((id, entry, candidate))
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        matches.retain(|(_, _, candidate)| candidate.primary_worktree);
    }
    match matches.as_slice() {
        [] => Ok(None),
        [(id, entry, _)] => vault_from_entry(id, entry).map(Some),
        _ => Err(MemoryError::AmbiguousVault),
    }
}

fn git_repository(start: &Path) -> Option<GitRepository> {
    let canonical_start = fs::canonicalize(start).ok()?;
    let worktree = canonical_start
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())?;
    let dot_git = worktree.join(".git");
    let metadata = fs::metadata(&dot_git).ok()?;
    let (git_dir, primary_worktree) = if metadata.is_dir() {
        (fs::canonicalize(&dot_git).ok()?, true)
    } else if metadata.is_file() {
        let pointer = read_small_text(&dot_git)?;
        let path = pointer.trim().strip_prefix("gitdir:")?.trim();
        if path.is_empty() {
            return None;
        }
        let path = Path::new(path);
        let resolved = if path.is_absolute() {
            path.to_owned()
        } else {
            worktree.join(path)
        };
        (fs::canonicalize(resolved).ok()?, false)
    } else {
        return None;
    };
    let common_dir_file = git_dir.join("commondir");
    let common_dir = if common_dir_file.is_file() {
        let path = read_small_text(&common_dir_file)?;
        let path = Path::new(path.trim());
        if path.as_os_str().is_empty() {
            return None;
        }
        let resolved = if path.is_absolute() {
            path.to_owned()
        } else {
            git_dir.join(path)
        };
        fs::canonicalize(resolved).ok()?
    } else {
        git_dir
    };
    Some(GitRepository {
        common_dir,
        primary_worktree,
    })
}

fn read_small_text(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_POINTER_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn default_id(project_path: &Path) -> String {
    let basename = project_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("vault");
    let slug = basename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let digest = Sha256::digest(project_path.as_os_str().as_encoded_bytes());
    let mut suffix = String::with_capacity(8);
    for byte in &digest[..4] {
        if write!(&mut suffix, "{byte:02x}").is_err() {
            break;
        }
    }
    format!("{}-{suffix}", if slug.is_empty() { "vault" } else { &slug })
}

fn validate_vault_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || id.starts_with('-')
        || id.ends_with('-')
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MemoryError::InvalidConfig(format!(
            "vault id `{id}` must use lowercase ASCII letters, digits, and internal hyphens"
        )));
    }
    Ok(())
}

fn path_matches_any(
    path: &Path,
    configured: &[PathBuf],
) -> Result<bool> {
    for prefix in configured {
        if !prefix.is_absolute() {
            return Err(MemoryError::InvalidConfig(format!(
                "hook policy path must be absolute: {}",
                prefix.display()
            )));
        }
        let canonical = fs::canonicalize(prefix).map_err(|error| MemoryError::io(prefix, error))?;
        if path.starts_with(canonical) {
            return Ok(true);
        }
    }
    Ok(false)
}

const fn default_true() -> bool {
    true
}
