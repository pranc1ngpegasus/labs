//! `koe list` — enumerate capture-able apps.

use clap::Parser;
use koe_core::{enumerate_apps, native_provider_registered};

use super::Run;
use super::apps_table::{format_apps_json, format_apps_table, prepare_apps};
use crate::MainError;

/// List capture-able apps and their audio activity.
///
/// Device enumeration is intentionally out of scope: the CLI surface for
/// `koe list` only exposes app rows (`--audio-only` / `--json`). Default
/// devices belong on `koe info` (task 27).
#[derive(Debug, Parser)]
pub struct ListArgs {
    /// Only show apps with active audio.
    #[arg(long)]
    audio_only: bool,

    /// Output as a JSON array.
    #[arg(long)]
    json: bool,
}

impl Run for ListArgs {
    fn run(
        self,
        _config: &crate::config::KoeConfig,
    ) -> Result<(), MainError> {
        if !native_provider_registered() {
            return Err(MainError::NativeBridgeUnavailable("list"));
        }

        let enumerated = enumerate_apps();
        if enumerated.is_empty() {
            eprintln!("note: no capture-able apps reported by the native provider");
        }
        let apps = prepare_apps(enumerated, self.audio_only);
        if self.json {
            println!("{}", format_apps_json(&apps)?);
        } else {
            print!("{}", format_apps_table(&apps));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_errors_without_native_provider() {
        let err = ListArgs {
            audio_only: false,
            json: false,
        }
        .run(&crate::config::KoeConfig::default())
        .expect_err("must fail without provider");
        assert!(matches!(err, MainError::NativeBridgeUnavailable("list")));
    }
}
