mod capture;
mod error;
mod fsutil;
mod hook;
mod index;
mod model;
mod mutation;
mod skill;
mod vault;

use std::{
    env,
    io::{self, Read},
    path::PathBuf,
};

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

pub use capture::{CaptureEvent, CaptureResult, EVENT_SCHEMA};
pub use error::MemoryError;
pub use index::{
    Diagnostic, DoctorDiagnostic, DoctorReport, Edge, NoteSummary, SearchHit, SyncReport,
};
pub use model::{
    Dependency, Frontmatter, Link, MAX_FRONTMATTER_BYTES, MAX_NOTE_BYTES, Note, NoteState,
    NoteType, Relation, SCHEMA, Source,
};
pub use mutation::{MutationResult, PromotionProposal, PromotionResult};
pub use skill::{MEMORY_OPENAI_YAML, MEMORY_SKILL, MEMORY_SKILL_FILES, MEMORY_SKILL_MD};
pub use vault::{MemoryHome, Registry, Vault, VaultEntry};

use crate::error::Result;

/// Command-line configuration for `ren memory`.
#[derive(Args, Debug)]
#[command(arg_required_else_help = true, subcommand_required = true)]
pub struct Config {
    #[command(subcommand)]
    command: Option<MemoryCommand>,
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// Initializes user-scoped memory and registers a vault for a project.
    Init(InitArgs),
    /// Installs, inspects, or removes coding-agent lifecycle hooks.
    Hook(HookArgs),
    /// Normalizes and captures a coding-agent hook payload from stdin.
    IngestHook(IngestHookArgs),
    /// Captures a fleeting note from an argument or stdin.
    Capture(CaptureArgs),
    /// Incrementally synchronizes Markdown into the `SQLite` projection.
    Sync(VaultArgs),
    /// Indexes a vault, optionally rebuilding all derived state.
    Index(IndexArgs),
    /// Lists indexed notes.
    List(ListArgs),
    /// Prints a note's source Markdown.
    Show(IdArgs),
    /// Searches title, body, aliases, and tags.
    Search(SearchArgs),
    /// Prints a note's dependencies.
    Deps(IdArgs),
    /// Prints notes that depend on a note.
    Refs(IdArgs),
    /// Prints all dependency and knowledge backlinks.
    Backlinks(IdArgs),
    /// Traverses accepted knowledge links to a bounded depth.
    Related(RelatedArgs),
    /// Finds the shortest undirected knowledge-link path between two notes.
    Path(PathArgs),
    /// Lists notes with no incoming or outgoing edges.
    Orphans(VaultArgs),
    /// Proposes or explicitly applies promotion to permanent notes.
    Promote(PromoteArgs),
    /// Adds an accepted typed link.
    Link(LinkArgs),
    /// Revises a note's title or body.
    Revise(ReviseArgs),
    /// Moves a note to the archive without deleting its history.
    Archive(IdArgs),
    /// Exports a vault as Markdown, JSON, or Graphviz DOT.
    Export(ExportArgs),
    /// Diagnoses the Markdown graph and disposable index.
    Doctor(VaultArgs),
}

#[derive(Clone, Debug, Args)]
struct VaultArgs {
    /// Selects a registered vault; inferred from the current directory by default.
    #[arg(long, value_name = "ID")]
    vault: Option<String>,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Initializes user scope. Memory is user-scoped in this release.
    #[arg(long)]
    user: bool,
    /// Stable vault identifier; generated from the project path by default.
    #[arg(long, value_name = "ID")]
    vault: Option<String>,
    /// Markdown vault root; defaults under ~/.ren/memory/vaults.
    #[arg(long, value_name = "PATH")]
    path: Option<PathBuf>,
    /// Project directory associated with the vault; defaults to the current directory.
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true, subcommand_required = true)]
struct HookArgs {
    #[command(subcommand)]
    command: Option<HookCommand>,
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Merges the ren-owned hook into the agent's user configuration.
    Install(HookTargetArgs),
    /// Reports whether the ren-owned hook is installed.
    Status(HookTargetArgs),
    /// Removes only the ren-owned hook.
    Uninstall(HookTargetArgs),
}

#[derive(Clone, Debug, Args)]
struct HookTargetArgs {
    /// Agent adapter. Codex is the initial supported adapter.
    #[arg(long)]
    agent: String,
    /// Operates on user configuration.
    #[arg(long)]
    user: bool,
}

#[derive(Debug, Args)]
struct IngestHookArgs {
    #[arg(long)]
    agent: String,
    #[arg(long)]
    event: String,
    /// Suppresses stdout for lifecycle-hook execution.
    #[arg(long, hide = true)]
    quiet: bool,
}

#[derive(Debug, Args)]
struct CaptureArgs {
    #[command(flatten)]
    vault: VaultArgs,
    /// Optional title. Otherwise the first non-empty content line is used.
    #[arg(long)]
    title: Option<String>,
    /// Note content. Reads stdin when omitted.
    #[arg(value_name = "CONTENT")]
    content: Option<String>,
}

#[derive(Debug, Args)]
struct IndexArgs {
    #[command(flatten)]
    vault: VaultArgs,
    /// Deletes and reconstructs all disposable projection state.
    #[arg(long)]
    rebuild: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    vault: VaultArgs,
    #[arg(long = "type", value_enum)]
    note_type: Option<NoteType>,
    #[arg(long, value_enum)]
    state: Option<NoteState>,
}

#[derive(Debug, Args)]
struct IdArgs {
    id: String,
    #[command(flatten)]
    vault: VaultArgs,
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: String,
    #[command(flatten)]
    vault: VaultArgs,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=1000))]
    limit: u16,
}

#[derive(Debug, Args)]
struct RelatedArgs {
    id: String,
    #[command(flatten)]
    vault: VaultArgs,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=32))]
    depth: u8,
}

#[derive(Debug, Args)]
struct PathArgs {
    from: String,
    to: String,
    #[command(flatten)]
    vault: VaultArgs,
}

#[derive(Debug, Args)]
struct PromoteArgs {
    ids: Vec<String>,
    #[command(flatten)]
    vault: VaultArgs,
    /// Explicitly accepts and applies the displayed proposal.
    #[arg(long)]
    apply: bool,
    /// Applies one previously returned proposal operation.
    #[arg(long, requires = "apply", value_name = "KEY")]
    operation: Option<String>,
}

#[derive(Debug, Args)]
struct LinkArgs {
    from: String,
    #[arg(value_enum)]
    relation: Relation,
    to: String,
    #[command(flatten)]
    vault: VaultArgs,
    /// Human-readable evidence for the accepted relation.
    #[arg(long)]
    reason: String,
}

#[derive(Debug, Args)]
struct ReviseArgs {
    id: String,
    #[command(flatten)]
    vault: VaultArgs,
    #[arg(long, conflicts_with = "clear_title")]
    title: Option<String>,
    #[arg(long, conflicts_with = "title")]
    clear_title: bool,
    #[arg(long)]
    body: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExportFormat {
    Markdown,
    Json,
    Dot,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(flatten)]
    vault: VaultArgs,
    #[arg(long, value_enum, default_value_t = ExportFormat::Markdown)]
    format: ExportFormat,
}

/// Dispatches a `memory` subcommand.
///
/// # Errors
///
/// Returns validation, filesystem, or `SQLite` failures from the selected operation.
pub fn run(config: Config) -> std::result::Result<(), MemoryError> {
    match config.command {
        Some(MemoryCommand::Init(args)) => run_init(&args),
        Some(MemoryCommand::Hook(args)) => run_hook(&args),
        Some(MemoryCommand::IngestHook(args)) => run_ingest_hook(&args),
        Some(MemoryCommand::Capture(args)) => run_capture(&args),
        Some(MemoryCommand::Sync(args)) => run_sync(&args, false),
        Some(MemoryCommand::Index(args)) => run_sync(&args.vault, args.rebuild),
        Some(MemoryCommand::List(args)) => run_list(&args),
        Some(MemoryCommand::Show(args)) => run_show(&args),
        Some(MemoryCommand::Search(args)) => run_search(&args),
        Some(MemoryCommand::Deps(args)) => run_edge_query(&args, EdgeQuery::Deps),
        Some(MemoryCommand::Refs(args)) => run_edge_query(&args, EdgeQuery::Refs),
        Some(MemoryCommand::Backlinks(args)) => run_edge_query(&args, EdgeQuery::Backlinks),
        Some(MemoryCommand::Related(args)) => run_related(&args),
        Some(MemoryCommand::Path(args)) => run_path(&args),
        Some(MemoryCommand::Orphans(args)) => run_orphans(&args),
        Some(MemoryCommand::Promote(args)) => run_promote(&args),
        Some(MemoryCommand::Link(args)) => run_link(&args),
        Some(MemoryCommand::Revise(args)) => run_revise(&args),
        Some(MemoryCommand::Archive(args)) => run_archive(&args),
        Some(MemoryCommand::Export(args)) => run_export(&args),
        Some(MemoryCommand::Doctor(args)) => run_doctor(&args),
        None => Err(MemoryError::InvalidConfig(
            "a memory subcommand is required".into(),
        )),
    }
}

fn run_init(args: &InitArgs) -> Result<()> {
    if !args.user {
        return Err(MemoryError::InvalidConfig(
            "memory initialization requires --user".into(),
        ));
    }
    let home = MemoryHome::discover()?;
    let project = args
        .project
        .clone()
        .unwrap_or(env::current_dir().map_err(|error| MemoryError::io(".", error))?);
    let vault = home.register(args.vault.as_deref(), args.path.as_deref(), &project)?;
    print_json(&serde_json::json!({
        "initialized": true,
        "scope": "user",
        "vault": vault.id,
        "root": vault.root,
        "project_path": vault.project_path,
    }))
}

fn run_hook(args: &HookArgs) -> Result<()> {
    let command = args
        .command
        .as_ref()
        .ok_or_else(|| MemoryError::InvalidConfig("a hook subcommand is required".into()))?;
    let target = match command {
        HookCommand::Install(target)
        | HookCommand::Status(target)
        | HookCommand::Uninstall(target) => target,
    };
    validate_hook_target(target)?;
    let status = match command {
        HookCommand::Install(_) => hook::install_codex_user()?,
        HookCommand::Status(_) => hook::status_codex_user()?,
        HookCommand::Uninstall(_) => hook::uninstall_codex_user()?,
    };
    print_json(&status)
}

fn run_ingest_hook(args: &IngestHookArgs) -> Result<()> {
    let event = capture::read_event_stdin(&args.agent, &args.event)?;
    let home = MemoryHome::discover()?;
    home.initialize()?;
    let hint = capture::hint_path(&event);
    let vault = home.resolve_or_register_hint(hint)?;
    let result = capture::capture_event(&vault, &event)?;
    if args.quiet {
        Ok(())
    } else {
        print_json(&result)
    }
}

fn run_capture(args: &CaptureArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    let content = match &args.content {
        Some(content) => content.clone(),
        None => read_stdin_bounded(MAX_NOTE_BYTES)?,
    };
    let event = capture::manual_event(
        &vault,
        &content,
        args.title.as_deref(),
        jiff::Timestamp::now().to_string(),
    );
    print_json(&capture::capture_event(&vault, &event)?)
}

fn run_sync(
    args: &VaultArgs,
    rebuild: bool,
) -> Result<()> {
    let vault = resolve_vault(args)?;
    print_json(&index::sync(&vault, rebuild, true)?)
}

fn run_list(args: &ListArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&index::list(&vault, args.note_type, args.state)?)
}

fn run_show(args: &IdArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    let path = index::note_path(&vault, &args.id)?;
    let markdown = std::fs::read_to_string(&path).map_err(|error| MemoryError::io(&path, error))?;
    print!("{markdown}");
    Ok(())
}

fn run_search(args: &SearchArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&index::search(
        &vault,
        &args.query,
        usize::from(args.limit),
    )?)
}

#[derive(Clone, Copy)]
enum EdgeQuery {
    Deps,
    Refs,
    Backlinks,
}

fn run_edge_query(
    args: &IdArgs,
    query: EdgeQuery,
) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    let mut edges = match query {
        EdgeQuery::Deps => index::edges_from(&vault, &args.id)?,
        EdgeQuery::Refs => index::edges_to(&vault, &args.id)?,
        EdgeQuery::Backlinks => index::backlinks(&vault, &args.id)?,
    };
    match query {
        EdgeQuery::Deps | EdgeQuery::Refs => {
            edges.retain(|edge| edge.relation == "depends_on");
        },
        EdgeQuery::Backlinks => {},
    }
    print_json(&edges)
}

fn run_related(args: &RelatedArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&index::related(&vault, &args.id, usize::from(args.depth))?)
}

fn run_path(args: &PathArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&index::shortest_path(&vault, &args.from, &args.to)?)
}

fn run_orphans(args: &VaultArgs) -> Result<()> {
    let vault = resolve_vault(args)?;
    print_json(&index::orphans(&vault)?)
}

fn run_promote(args: &PromoteArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&mutation::promote_operation(
        &vault,
        &args.ids,
        args.operation.as_deref(),
        args.apply,
        &jiff::Timestamp::now().to_string(),
    )?)
}

fn run_link(args: &LinkArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&mutation::link(
        &vault,
        &args.from,
        args.relation,
        &args.to,
        &args.reason,
        &jiff::Timestamp::now().to_string(),
    )?)
}

fn run_revise(args: &ReviseArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    let title = if args.clear_title {
        mutation::TitleChange::Clear
    } else if let Some(title) = &args.title {
        mutation::TitleChange::Set(title.clone())
    } else {
        mutation::TitleChange::Unchanged
    };
    print_json(&mutation::revise(
        &vault,
        &args.id,
        title,
        args.body.clone(),
        &jiff::Timestamp::now().to_string(),
    )?)
}

fn run_archive(args: &IdArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    print_json(&mutation::archive(
        &vault,
        &args.id,
        &jiff::Timestamp::now().to_string(),
    )?)
}

fn run_export(args: &ExportArgs) -> Result<()> {
    let vault = resolve_vault(&args.vault)?;
    match args.format {
        ExportFormat::Markdown => print!("{}", mutation::export_markdown(&vault)?),
        ExportFormat::Json => print_json(&mutation::export_json(&vault)?)?,
        ExportFormat::Dot => print!("{}", mutation::export_dot(&vault)?),
    }
    Ok(())
}

fn run_doctor(args: &VaultArgs) -> Result<()> {
    let vault = resolve_vault(args)?;
    print_json(&index::doctor(&vault)?)
}

fn resolve_vault(args: &VaultArgs) -> Result<Vault> {
    let home = MemoryHome::discover()?;
    let cwd = env::current_dir().map_err(|error| MemoryError::io(".", error))?;
    home.resolve(args.vault.as_deref(), &cwd)
}

fn validate_hook_target(target: &HookTargetArgs) -> Result<()> {
    if target.agent != "codex" {
        return Err(MemoryError::InvalidConfig(format!(
            "unsupported hook agent `{}`; supported: codex",
            target.agent
        )));
    }
    if !target.user {
        return Err(MemoryError::InvalidConfig(
            "hook management currently requires --user".into(),
        ));
    }
    Ok(())
}

fn read_stdin_bounded(limit: usize) -> Result<String> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(u64::try_from(limit + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| MemoryError::io("<stdin>", error))?;
    if bytes.len() > limit {
        return Err(MemoryError::InputTooLarge { limit });
    }
    String::from_utf8(bytes)
        .map_err(|_| MemoryError::Validation("stdin must be valid UTF-8".into()))
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests;
