use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{WorkflowError, WorkflowMeta, bundled, meta};

/// The origin a workflow was discovered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowSource {
    /// A workflow found under `<repo-root-or-cwd>/.ren/workflows`.
    Project,
    /// A workflow found under `~/.ren/workflows`.
    User,
    /// An official workflow embedded in the binary.
    Bundled,
}

/// A discovered workflow along with its extracted metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredWorkflow {
    /// Invocation handle (`meta.name`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Guidance describing when to select this workflow.
    pub when_to_use: Option<String>,
    /// Filesystem path, or a display-only `bundled/<name>.rhai` location.
    pub path: PathBuf,
    /// Workflow origin.
    pub source: WorkflowSource,
}

/// A non-fatal problem encountered while scanning a workflow source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryWarning {
    /// The offending file or bundled display location.
    pub path: PathBuf,
    /// Human-readable reason the workflow was skipped.
    pub reason: String,
}

/// The full outcome of a discovery pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    /// Successfully discovered workflows in precedence order.
    pub workflows: Vec<DiscoveredWorkflow>,
    /// Non-fatal warnings for skipped workflows.
    pub warnings: Vec<DiscoveryWarning>,
}

/// Returns the project workflow directory for the current working directory.
///
/// The repo root is the nearest ancestor of the working directory containing a
/// `.git` entry; the working directory itself is used when none is found.
///
/// # Errors
///
/// Returns [`WorkflowError::Io`] when the current directory cannot be resolved.
pub fn project_workflow_dir() -> Result<PathBuf, WorkflowError> {
    let cwd = std::env::current_dir().map_err(|error| WorkflowError::io(".", error))?;
    Ok(project_workflow_dir_in(&cwd))
}

#[must_use]
pub fn project_workflow_dir_in(start: &Path) -> PathBuf {
    repo_root(start).join(".ren").join("workflows")
}

/// Returns the user workflow directory (`~/.ren/workflows`) when `$HOME` is set.
///
/// This Unix-oriented resolver intentionally does not consult Windows
/// `%USERPROFILE%`.
#[must_use]
pub fn user_workflow_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(user_workflow_dir_in(Path::new(&home)))
}

#[must_use]
pub fn user_workflow_dir_in(home: &Path) -> PathBuf {
    home.join(".ren").join("workflows")
}

#[must_use]
pub fn repo_root(start: &Path) -> PathBuf {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    start.to_path_buf()
}

/// Discovers project, user, and bundled workflows.
///
/// Entries use project > user > bundled precedence. Files that cannot be
/// parsed are skipped and reported as warnings rather than failing the scan.
///
/// # Errors
///
/// Returns [`WorkflowError::Io`] when the working directory cannot be resolved.
pub fn discover() -> Result<Discovery, WorkflowError> {
    let project_dir = project_workflow_dir()?;
    Ok(discover_in(&project_dir, user_workflow_dir().as_deref()))
}

/// Discovers workflows from explicit project and user directories plus the
/// embedded official bundle.
///
/// Entries use project > user > bundled precedence.
#[must_use]
pub fn discover_in(
    project_dir: &Path,
    user_dir: Option<&Path>,
) -> Discovery {
    let mut discovery = Discovery::default();
    scan_dir(project_dir, WorkflowSource::Project, &mut discovery);
    if let Some(user_dir) = user_dir {
        scan_dir(user_dir, WorkflowSource::User, &mut discovery);
    }
    scan_bundled(&mut discovery);
    discovery
}

/// Resolves a workflow by name, honouring project > user > bundled precedence.
///
/// # Errors
///
/// Returns [`WorkflowError::InvalidConfig`] when no workflow with the given name
/// is discovered, or [`WorkflowError::Io`] when discovery fails.
pub fn resolve(name: &str) -> Result<DiscoveredWorkflow, WorkflowError> {
    resolve_in(name, &discover()?)
}

/// Resolves a workflow name against an already-computed discovery.
///
/// # Errors
///
/// Returns [`WorkflowError::InvalidConfig`] when no workflow matches.
pub fn resolve_in(
    name: &str,
    discovery: &Discovery,
) -> Result<DiscoveredWorkflow, WorkflowError> {
    discovery
        .workflows
        .iter()
        .find(|workflow| workflow.name == name)
        .cloned()
        .ok_or_else(|| {
            WorkflowError::InvalidConfig(format!("no workflow named `{name}` was discovered"))
        })
}

/// Loads the source for a discovered filesystem or bundled workflow.
///
/// # Errors
///
/// Returns [`WorkflowError::Io`] when a filesystem workflow cannot be read, or
/// [`WorkflowError::BundledWorkflowNotFound`] if embedded data is inconsistent.
pub fn load_source(workflow: &DiscoveredWorkflow) -> Result<String, WorkflowError> {
    match workflow.source {
        WorkflowSource::Project | WorkflowSource::User => fs::read_to_string(&workflow.path)
            .map_err(|error| WorkflowError::io(&workflow.path, error)),
        WorkflowSource::Bundled => bundled::find(&workflow.name)
            .map(|bundled| bundled.source.to_owned())
            .ok_or_else(|| WorkflowError::BundledWorkflowNotFound(workflow.name.clone())),
    }
}

fn scan_dir(
    dir: &Path,
    source: WorkflowSource,
    discovery: &mut Discovery,
) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut paths = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rhai"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        match load_meta(&path) {
            Ok(metadata) => add_workflow(path, source, metadata, discovery),
            Err(error) => discovery.warnings.push(DiscoveryWarning {
                path,
                reason: error.to_string(),
            }),
        }
    }
}

fn scan_bundled(discovery: &mut Discovery) {
    for workflow in bundled::WORKFLOWS {
        let path = PathBuf::from("bundled").join(workflow.file_name);
        match meta::extract(workflow.source) {
            Ok(metadata) => add_workflow(path, WorkflowSource::Bundled, metadata, discovery),
            Err(error) => discovery.warnings.push(DiscoveryWarning {
                path,
                reason: error.to_string(),
            }),
        }
    }
}

fn add_workflow(
    path: PathBuf,
    source: WorkflowSource,
    metadata: WorkflowMeta,
    discovery: &mut Discovery,
) {
    if let Some(existing) = discovery
        .workflows
        .iter()
        .find(|existing| existing.name == metadata.name)
    {
        // A name already claimed by a higher-precedence entry. An
        // intra-source collision is an operator mistake and is surfaced;
        // cross-source shadowing is legitimate and stays quiet.
        if existing.source == source {
            discovery.warnings.push(DiscoveryWarning {
                path,
                reason: format!(
                    "duplicate meta.name `{}`; already provided by {}",
                    metadata.name,
                    existing.path.display()
                ),
            });
        }
        return;
    }
    // Only warn about a retained entry, so a shadowed file never produces
    // noise about a workflow that is discarded.
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        && stem != metadata.name
    {
        discovery.warnings.push(DiscoveryWarning {
            path: path.clone(),
            reason: format!(
                "meta.name `{}` does not match file stem `{stem}`",
                metadata.name
            ),
        });
    }
    discovery.workflows.push(DiscoveredWorkflow {
        name: metadata.name,
        description: metadata.description,
        when_to_use: metadata.when_to_use,
        path,
        source,
    });
}

/// Reads a workflow file and extracts its metadata without running the script.
///
/// # Errors
///
/// Returns [`WorkflowError::Io`] when the file cannot be read or
/// [`WorkflowError::InvalidMeta`] when the metadata is invalid.
pub fn load_meta(path: &Path) -> Result<WorkflowMeta, WorkflowError> {
    let script = fs::read_to_string(path).map_err(|error| WorkflowError::io(path, error))?;
    meta::extract(&script)
}
