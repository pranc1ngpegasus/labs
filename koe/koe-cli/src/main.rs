//! koe-cli — command-line interface for Koe.

mod commands;
mod config;
mod progress;
mod signals;

use std::path::PathBuf;

use thiserror::Error;
use usage::{Cli, Subcommands};

use commands::{InfoArgs, ListArgs, PermissionsArgs, RecordArgs, Run, TranscribeArgs};
use config::KoeConfig;

#[derive(Debug, Cli)]
#[usage(
    bin = "koe",
    version = env!("CARGO_PKG_VERSION"),
    about = "Capture, transcribe, and inspect system audio on macOS",
    arg_required_else_help,
)]
struct CliRoot {
    /// Path to config file (default: ~/.config/koe/config.toml).
    #[usage(long, global)]
    config: Option<PathBuf>,

    #[usage(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommands)]
enum Command {
    /// Start a recording with optional transcription.
    Record(RecordArgs),
    /// List capture-able apps and audio activity.
    List(ListArgs),
    /// Transcribe an existing audio file (offline).
    Transcribe(TranscribeArgs),
    /// Check and diagnose macOS permissions.
    Permissions(PermissionsArgs),
    /// Show build and host system information.
    Info(InfoArgs),
}

#[derive(Debug, Error)]
pub(crate) enum MainError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// No `NativeProvider` is registered in this process.
    #[error(
        "native provider is not registered\n\
         `{0}` requires a registered NativeProvider \
         (macOS discovery shim failed to install, or register_native_provider was never called)"
    )]
    NativeBridgeUnavailable(&'static str),

    #[error("one or more permissions are not authorized")]
    PermissionsNotAuthorized,

    #[error("permission denied: {0} (tip: run `koe permissions`)")]
    PermissionDenied(String),

    #[error("invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("capture error: {0}")]
    Capture(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("interrupted")]
    Interrupted,

    #[error("internal error: {0}")]
    Internal(String),
}

impl MainError {
    /// Process exit code per the CLI interface spec.
    const fn exit_code(&self) -> i32 {
        match self {
            Self::PermissionDenied(_) | Self::PermissionsNotAuthorized => 1,
            Self::InvalidArgs(_) | Self::Json(_) => 2,
            Self::Capture(_) => 3,
            Self::Io(_) => 4,
            Self::Interrupted => 5,
            Self::NativeBridgeUnavailable(_) | Self::Internal(_) => 6,
        }
    }
}

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(error) => {
            // Interrupted is an expected exit path; keep the message short.
            if !matches!(error, MainError::Interrupted) {
                eprintln!("{error}");
            }
            error.exit_code()
        },
    };
    std::process::exit(code);
}

fn run() -> Result<(), MainError> {
    let _ = koe_core::install_default_native_provider();

    let cli = CliRoot::parse();
    let config = load_config(cli.config.as_deref())?;
    match cli.command {
        Command::Record(args) => args.run(&config),
        Command::List(args) => args.run(&config),
        Command::Transcribe(args) => args.run(&config),
        Command::Permissions(args) => args.run(&config),
        Command::Info(args) => args.run(&config),
    }
}

fn load_config(explicit: Option<&std::path::Path>) -> Result<KoeConfig, MainError> {
    config::load(explicit).map_err(MainError::InvalidArgs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn argv<'a>(args: &'a [&'a str]) -> Vec<&'a OsStr> {
        args.iter().map(|&s| OsStr::new(s)).collect()
    }

    #[test]
    fn parses_record_list_sources() {
        let cli =
            CliRoot::try_parse_from(&argv(&["koe", "record", "--list-sources"])).expect("parse");
        assert!(matches!(cli.command, Command::Record(_)));
    }

    #[test]
    fn parses_global_config_flag() {
        let cli = CliRoot::try_parse_from(&argv(&[
            "koe",
            "--config",
            "/tmp/other.toml",
            "record",
            "--list-sources",
        ]))
        .expect("parse");
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/other.toml")));
        assert!(matches!(cli.command, Command::Record(_)));
    }

    #[test]
    fn parses_list_flags() {
        let cli = CliRoot::try_parse_from(&argv(&["koe", "list", "--audio-only", "--json"]))
            .expect("parse");
        assert!(matches!(cli.command, Command::List(_)));
    }

    #[test]
    fn parses_transcribe_flags() {
        let cli = CliRoot::try_parse_from(&argv(&[
            "koe",
            "transcribe",
            "--format",
            "srt",
            "--locale",
            "ja-JP",
            "--start-at",
            "30s",
            "meeting.ogg",
        ]))
        .expect("parse");
        assert!(matches!(cli.command, Command::Transcribe(_)));
    }

    #[test]
    fn parses_permissions_check() {
        let cli = CliRoot::try_parse_from(&argv(&["koe", "permissions", "--check", "--json"]))
            .expect("parse");
        assert!(matches!(cli.command, Command::Permissions(_)));
    }

    #[test]
    fn parses_info() {
        let cli = CliRoot::try_parse_from(&argv(&["koe", "info", "--json"])).expect("parse");
        assert!(matches!(cli.command, Command::Info(_)));
    }

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(MainError::PermissionDenied("mic".into()).exit_code(), 1);
        assert_eq!(MainError::InvalidArgs("bad".into()).exit_code(), 2);
        assert_eq!(MainError::Capture("tap".into()).exit_code(), 3);
        assert_eq!(MainError::Io("disk".into()).exit_code(), 4);
        assert_eq!(MainError::Interrupted.exit_code(), 5);
        assert_eq!(MainError::Internal("x".into()).exit_code(), 6);
    }
}
