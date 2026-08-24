#![allow(clippy::pub_underscore_fields)] // usage-derive emits `__given_*` tracking fields

pub(crate) mod bridge;
mod bundled;
mod create;
mod engine;
mod error;
mod guide;
mod hash;
mod host;
mod init;
mod journal;
mod meta;
mod registry;
mod schema;
mod store;
mod value;

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
use usage::{Args, Subcommands};

pub use bridge::Agent;
pub use engine::{CompiledWorkflow, Engine, PauseInfo, RunOptions, RunResult};
pub use error::{HostError, WorkflowError};
pub use guide::{AUTHORING_MD, PROTOCOL_MD};
pub use host::{AgentOptions, AgentRequest, AgentResult, Capability, EchoHost, Host};
pub use init::{
    EmbeddedSkill, InitScope, OPENAI_YAML, SKILL_FILES, SKILL_MD, SkillDefinition, SkillFile,
    WORKFLOW_SKILL, install_skill, install_skills, skill_definition, skill_definition_for,
    supported_agents,
};
pub use journal::{Journal, JournalEntry, ParallelSlot};
pub use meta::{MetaPhase, WorkflowMeta};
pub use registry::{
    DiscoveredWorkflow, Discovery, DiscoveryWarning, WorkflowSource, discover, discover_in,
    load_meta, load_source, resolve, resolve_in, user_workflow_dir,
};
pub use schema::{tool_descriptor, validate_args};

/// Command-line configuration for the `workflow` command group.
#[derive(Args, Debug)]
#[usage(arg_required_else_help)]
pub struct Config {
    #[usage(subcommand)]
    command: WorkflowCommand,
}

impl Config {
    /// Builds a configuration that runs the given workflow.
    #[must_use]
    pub const fn for_run(args: RunArgs) -> Self {
        Self {
            command: WorkflowCommand::Run(args),
        }
    }
}

/// The individual `workflow` subcommands.
#[derive(Debug, Subcommands)]
enum WorkflowCommand {
    /// Runs a workflow by registry name or by path.
    Run(RunArgs),
    /// Lists discovered workflows.
    List,
    /// Prints a workflow's full metadata as JSON.
    Show(NameArgs),
    /// Prints an MCP-style tool descriptor for a workflow.
    Schema(NameArgs),
    /// Creates a workflow in the project or user store.
    Create(CreateArgs),
    /// Removes a workflow from the user workflow store.
    Remove(NameArgs),
    /// Installs the embedded skill into coding agents' skill directories.
    Init(InitArgs),
    /// Prints the embedded, version-matched agent guidance.
    Protocol(ProtocolArgs),
    /// Installs or uninstalls a slash-command dispatcher.
    Bridge(BridgeArgs),
}

/// Arguments accepted by `workflow protocol`.
#[derive(Args, Debug)]
pub struct ProtocolArgs {
    /// Prints the Rhai authoring reference instead of the execution protocol.
    #[usage(long)]
    authoring: bool,
}

/// Arguments accepted by `workflow init` (and the top-level `ren init`).
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Restricts installation to a single agent instead of every supported one.
    #[usage(long, value_enum, value_name = "AGENT")]
    agent: Option<bridge::Agent>,
    /// Installs into the home directory (the default).
    #[usage(long, conflicts("--project"))]
    user: bool,
    /// Installs into the current repository instead of the home directory.
    #[usage(long, conflicts("--user"))]
    project: bool,
    /// Overwrites existing skill files.
    #[usage(long)]
    force: bool,
}

/// Arguments accepted by `workflow create`.
#[derive(Args, Debug)]
struct CreateArgs {
    /// Name for the created workflow.
    #[usage(value_name = "NAME")]
    name: String,
    /// Writes to the project store (the default).
    #[usage(long, conflicts("--user"))]
    project: bool,
    /// Writes to the user store instead of the project store.
    #[usage(long, conflicts("--project"))]
    user: bool,
    /// Copies an official bundled workflow as the scaffold.
    #[usage(long, value_name = "BUNDLED_NAME")]
    from: Option<String>,
    /// Replaces an existing workflow file.
    #[usage(long)]
    force: bool,
}

/// Arguments accepted by `workflow bridge`.
#[derive(Args, Debug)]
struct BridgeArgs {
    #[usage(subcommand)]
    command: BridgeCommand,
}

#[derive(Debug, Subcommands)]
enum BridgeCommand {
    /// Installs one dispatcher command file.
    Install(BridgeInstallArgs),
    /// Removes the dispatcher command file.
    Uninstall(BridgeTargetArgs),
}

/// Common bridge target options.
#[derive(Args, Debug)]
struct BridgeTargetArgs {
    /// Agent whose command directory receives the dispatcher.
    #[usage(long, value_enum)]
    agent: bridge::Agent,
    /// Uses the agent's user-global command directory (the default).
    #[usage(long, conflicts("--project"))]
    global: bool,
    /// Uses the current repository's project command directory.
    #[usage(long, conflicts("--global"))]
    project: bool,
}

/// Bridge installation options.
#[derive(Args, Debug)]
struct BridgeInstallArgs {
    #[usage(flatten)]
    target: BridgeTargetArgs,
    /// Replaces an existing dispatcher file.
    #[usage(long)]
    force: bool,
}

/// Arguments accepted by `workflow run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Registry name or local script path; path mode may access files outside `.ren/workflows`.
    #[usage(value_name = "NAME_OR_PATH")]
    pub target: String,
    /// JSON value exposed to the workflow as `args`.
    #[usage(long, value_name = "JSON")]
    pub args: Option<String>,
    /// Maximum number of agent slots available to the run.
    #[usage(
        long,
        default = "128",
        validate = "int(value) >= 1 && int(value) <= 1024",
        validate_error = "must be between 1 and 1024"
    )]
    pub agent_budget: u16,
    /// Path to atomically checkpoint the journal after every committed effect.
    #[usage(long, value_name = "OUT")]
    pub journal: Option<PathBuf>,
    /// Journal to replay; successful calls are skipped and this file is updated unless --journal is set.
    #[usage(long, value_name = "IN")]
    pub resume: Option<PathBuf>,
    /// Rewinds the first failed or cancelled invocation before resuming.
    #[usage(long, requires("--resume"))]
    pub retry_failed: bool,
}

/// Arguments accepting a single workflow name.
#[derive(Args, Debug)]
pub struct NameArgs {
    /// Registry name of the workflow.
    #[usage(value_name = "NAME")]
    pub name: String,
}

/// Runs a workflow with any [`Host`].
///
/// This is the library entry point that callers can drive with a custom host.
/// Capability checks also apply to journal replay, so a resumed run's host must
/// grant at least every capability requested by calls in the journal. Replaying
/// a journal recorded under a stronger host with a weaker host is rejected.
///
/// # Errors
///
/// Returns any compilation or execution error surfaced by the engine.
pub fn run_with_host<H>(
    host: H,
    script: &str,
    options: RunOptions,
) -> Result<RunResult, WorkflowError>
where
    H: Host + 'static,
{
    Engine::new(host).run_script(script, options)
}

/// Dispatches a `workflow` subcommand.
///
/// # Errors
///
/// Returns an error when no subcommand is supplied or when the selected
/// subcommand fails.
pub fn run(config: Config) -> Result<(), WorkflowError> {
    match config.command {
        WorkflowCommand::Run(args) => run_workflow(&args),
        WorkflowCommand::List => list_workflows(),
        WorkflowCommand::Show(args) => show_workflow(&args.name),
        WorkflowCommand::Schema(args) => schema_workflow(&args.name),
        WorkflowCommand::Create(args) => create_workflow(&args),
        WorkflowCommand::Remove(args) => remove_workflow(&args.name),
        WorkflowCommand::Init(args) => run_init(&args),
        WorkflowCommand::Protocol(args) => run_protocol(&args),
        WorkflowCommand::Bridge(args) => run_bridge(&args),
    }
}

/// Prints the embedded, version-matched agent guidance.
///
/// By default this is the execution protocol also injected into every run result
/// as `agent_protocol`; with `--authoring` it is the Rhai authoring reference.
///
/// # Errors
///
/// This never fails; it returns `Result` for a uniform dispatch signature.
pub fn run_protocol(args: &ProtocolArgs) -> Result<(), WorkflowError> {
    let document = if args.authoring {
        guide::AUTHORING_MD
    } else {
        guide::PROTOCOL_MD
    };
    print!("{document}");
    Ok(())
}

/// Installs the embedded `ren-workflow` skill into agent skill directories.
///
/// With no `--agent`, the skill is installed for every supported agent. Files
/// land under `<base>/<agent>/skills/ren-workflow/`, where `<base>` is the home
/// directory (user scope, the default) or the repository root (`--project`).
///
/// # Errors
///
/// Returns [`WorkflowError::HomeUnavailable`] when user scope is requested but
/// `$HOME` is unset, [`WorkflowError::SkillExists`] when a file has different
/// contents without `--force`, [`WorkflowError::UnsafeSkillPath`] when a target
/// path contains a symbolic link or escapes its installation base, or
/// [`WorkflowError::Io`] on other filesystem failures.
pub fn run_init(args: &InitArgs) -> Result<(), WorkflowError> {
    run_init_with_skills(args, &[])
}

/// Installs `ren-workflow` plus additional embedded skills into coding agents.
///
/// This supports the top-level `ren init`, while `workflow init` remains
/// focused on the workflow skill.
///
/// # Errors
///
/// Returns the same target-resolution and installation errors as [`run_init`].
pub fn run_init_with_skills(
    args: &InitArgs,
    additional_skills: &[EmbeddedSkill],
) -> Result<(), WorkflowError> {
    let scope = if args.project {
        InitScope::Project
    } else {
        InitScope::User
    };
    let base = init_base(scope)?;
    let agents = args
        .agent
        .map_or_else(|| supported_agents().to_vec(), |agent| vec![agent]);
    let mut definitions = Vec::new();
    for agent in agents {
        for skill in std::iter::once(WORKFLOW_SKILL).chain(additional_skills.iter().copied()) {
            definitions.push((skill, skill_definition_for(&base, scope, agent, skill)));
        }
    }

    install_skills(
        &definitions
            .iter()
            .map(|(_, definition)| definition.clone())
            .collect::<Vec<_>>(),
        args.force,
    )?;

    for (skill, definition) in definitions {
        println!(
            "installed {} skill at {}",
            skill.name,
            definition.dir.display()
        );
    }
    Ok(())
}

fn init_base(scope: InitScope) -> Result<PathBuf, WorkflowError> {
    match scope {
        InitScope::User => {
            let home = std::env::var_os("HOME").ok_or(WorkflowError::HomeUnavailable)?;
            if home.is_empty() {
                return Err(WorkflowError::HomeUnavailable);
            }
            Ok(PathBuf::from(home))
        },
        InitScope::Project => {
            let cwd = std::env::current_dir().map_err(|error| WorkflowError::io(".", error))?;
            Ok(registry::repo_root(&cwd))
        },
    }
}

fn run_workflow(args: &RunArgs) -> Result<(), WorkflowError> {
    let script = load_target(&args.target)?;
    let mut parsed_args = args
        .args
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;

    let engine = Engine::new(EchoHost);
    let workflow = engine.compile(&script)?;
    if let Some(schema) = &workflow.metadata().args_schema {
        if parsed_args.is_none() && schema.get("type").and_then(Value::as_str) == Some("object") {
            parsed_args = Some(serde_json::json!({}));
        }
        validate_args(schema, parsed_args.as_ref())?;
    }

    let mut journal = match &args.resume {
        Some(path) => Journal::from_json(
            &fs::read_to_string(path).map_err(|error| WorkflowError::io(path, error))?,
        )?,
        None => Journal::new(),
    };
    if args.retry_failed {
        journal.retry_failed();
    }
    let checkpoint = args.journal.clone().or_else(|| args.resume.clone());
    if let Some(path) = &checkpoint {
        journal.write_atomic(path)?;
    }

    let result = engine.run(
        &workflow,
        RunOptions {
            args: parsed_args,
            journal,
            agent_budget: usize::from(args.agent_budget),
            checkpoint: checkpoint.clone(),
        },
    )?;

    if let Some(path) = &checkpoint {
        result.journal.write_atomic(path)?;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Loads a registry name or an explicit workflow path.
///
/// Path mode intentionally accepts arbitrary existing user-supplied paths,
/// including absolute paths and paths containing `..`; it is not confined to
/// `.ren/workflows`. Callers forwarding an untrusted `target` must validate or
/// reject path-mode input before invoking this function.
fn load_target(target: &str) -> Result<String, WorkflowError> {
    let path = Path::new(target);
    let looks_like_path = target.contains('/')
        || target.contains('\\')
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rhai"));
    if looks_like_path && path.is_file() {
        return fs::read_to_string(path).map_err(|error| WorkflowError::io(path, error));
    }
    let workflow = resolve(target)?;
    load_source(&workflow)
}

const fn source_label(source: WorkflowSource) -> &'static str {
    match source {
        WorkflowSource::Project => "project",
        WorkflowSource::User => "user/store",
        WorkflowSource::Bundled => "bundled",
    }
}

fn list_workflows() -> Result<(), WorkflowError> {
    let discovery = discover()?;
    for workflow in &discovery.workflows {
        let source = source_label(workflow.source);
        println!(
            "{}\t{}\t{}\t{}",
            workflow.name,
            workflow.description,
            source,
            workflow.path.display()
        );
    }
    for warning in &discovery.warnings {
        eprintln!(
            "warning: skipped {}: {}",
            warning.path.display(),
            warning.reason
        );
    }
    Ok(())
}

fn show_workflow(name: &str) -> Result<(), WorkflowError> {
    let workflow = resolve(name)?;
    let metadata = meta::extract(&load_source(&workflow)?)?;
    println!("{}", serde_json::to_string_pretty(&metadata)?);
    Ok(())
}

fn schema_workflow(name: &str) -> Result<(), WorkflowError> {
    let workflow = resolve(name)?;
    let metadata = meta::extract(&load_source(&workflow)?)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&tool_descriptor(&metadata))?
    );
    Ok(())
}

fn create_workflow(args: &CreateArgs) -> Result<(), WorkflowError> {
    let target = match (args.project, args.user) {
        (false, true) => create::CreateTarget::User,
        (_, false) => create::CreateTarget::Project,
        (true, true) => {
            return Err(WorkflowError::InvalidConfig(
                "--project and --user cannot be used together".into(),
            ));
        },
    };
    let cwd = std::env::current_dir().map_err(|error| WorkflowError::io(".", error))?;
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let store = create::store_dir(&cwd, home.as_deref(), target)?;
    let plan = create::create_in(&store, &args.name, args.from.as_deref(), args.force)?;
    println!("created {} at {}", args.name, plan.path.display());
    Ok(())
}

fn remove_workflow(name: &str) -> Result<(), WorkflowError> {
    let store = user_workflow_dir().ok_or(WorkflowError::HomeUnavailable)?;
    let removed = store::remove_from_store(name, &store)?;
    println!("removed {name} from {}", removed.display());
    Ok(())
}

const fn bridge_scope(target: &BridgeTargetArgs) -> bridge::BridgeScope {
    // usage rejects `--global --project`; neither flag means the global default.
    if target.project {
        bridge::BridgeScope::Project
    } else {
        bridge::BridgeScope::Global
    }
}

fn bridge_base(scope: bridge::BridgeScope) -> Result<PathBuf, WorkflowError> {
    match scope {
        bridge::BridgeScope::Global => {
            let home = std::env::var_os("HOME").ok_or(WorkflowError::HomeUnavailable)?;
            if home.is_empty() {
                return Err(WorkflowError::HomeUnavailable);
            }
            Ok(PathBuf::from(home))
        },
        bridge::BridgeScope::Project => {
            let cwd = std::env::current_dir().map_err(|error| WorkflowError::io(".", error))?;
            Ok(registry::repo_root(&cwd))
        },
    }
}

fn run_bridge(args: &BridgeArgs) -> Result<(), WorkflowError> {
    match &args.command {
        BridgeCommand::Install(args) => {
            let scope = bridge_scope(&args.target);
            let definition =
                bridge::bridge_definition(&bridge_base(scope)?, args.target.agent, scope);
            bridge::install_bridge(&definition, args.force)?;
            println!("installed bridge at {}", definition.path.display());
        },
        BridgeCommand::Uninstall(target) => {
            let scope = bridge_scope(target);
            let definition = bridge::bridge_definition(&bridge_base(scope)?, target.agent, scope);
            if bridge::uninstall_bridge(&definition)? {
                println!("removed bridge at {}", definition.path.display());
            } else {
                println!("no bridge installed at {}", definition.path.display());
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests;
