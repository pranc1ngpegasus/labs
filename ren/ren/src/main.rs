use clap::{Parser, Subcommand};
use std::process::ExitCode;
use thiserror::Error;

#[derive(Debug, Parser)]
#[clap(
    name = env!("CARGO_PKG_NAME"),
    version = env!("CARGO_PKG_VERSION"),
    arg_required_else_help = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Workflow(ren_workflow::Config),
    /// Captures, indexes, and queries local Markdown knowledge.
    Memory(ren_memory::Config),
    /// Installs every embedded skill into coding agents.
    Init(ren_workflow::InitArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Workflow(config) => ren_workflow::run(config).map_err(CommandError::from),
        Command::Memory(config) => ren_memory::run(config).map_err(CommandError::from),
        // The top-level `init` recursively installs every embedded skill.
        Command::Init(args) => {
            ren_workflow::run_init_with_skills(&args, &[ren_memory::MEMORY_SKILL])
                .map_err(CommandError::from)
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({
                    "error": {
                        "class": error.class(),
                        "message": error.to_string()
                    }
                })
            );
            ExitCode::FAILURE
        },
    }
}

/// Common error type for the `ren` CLI, unifying failures from the workflow and
/// memory subsystems so they can be reported and converted in one place.
#[derive(Debug, Error)]
enum CommandError {
    #[error(transparent)]
    Workflow(#[from] ren_workflow::WorkflowError),
    #[error(transparent)]
    Memory(#[from] ren_memory::MemoryError),
}

impl CommandError {
    /// Returns a stable error class for JSON error output.
    #[must_use]
    const fn class(&self) -> &'static str {
        match self {
            Self::Workflow(error) => error.class(),
            Self::Memory(error) => error.class(),
        }
    }
}
