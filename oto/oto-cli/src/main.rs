//! Oto CLI — command-line interface for Oto.

mod commands;

use std::process::ExitCode;

use thiserror::Error;
use usage::{Cli, Subcommands};

use commands::{ListArgs, Run};

#[derive(Debug, Cli)]
#[usage(
    bin = "oto",
    version = env!("CARGO_PKG_VERSION"),
    about = "Offline audio recorder — microphone capture to WAV or Ogg/Opus",
    arg_required_else_help,
)]
struct CliRoot {
    #[usage(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommands)]
enum Command {
    /// List input audio devices.
    List(ListArgs),
}

/// Errors surfaced by `oto` subcommands.
///
/// Exit-code mapping is stable (design 03): 2 = arguments, 3 = capture,
/// 4 = I/O, 5 = interrupt, 6 = internal. Variants are added as the commands
/// that produce them land.
#[derive(Debug, Error)]
pub(crate) enum MainError {
    /// Device enumeration or capture failure.
    #[error("capture error: {0}")]
    Capture(String),

    /// Internal error.
    #[error("internal error: {0}")]
    Internal(String),
}

impl MainError {
    /// Process exit code per design 03.
    pub(crate) const fn exit_code(&self) -> u8 {
        match self {
            Self::Capture(_) => 3,
            Self::Internal(_) => 6,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        },
    }
}

fn run() -> Result<(), MainError> {
    match CliRoot::parse().command {
        Command::List(args) => args.run(),
    }
}
