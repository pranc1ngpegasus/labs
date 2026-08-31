//! `oto record` — capture microphone or system audio to a WAV/Ogg/Opus file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oto_core::{
    AudioSource, OutputFormat, RecordingConfig, RecordingError, RecordingSession, Tags,
    list_input_devices,
};
use usage::Args;

use super::Run;
use crate::MainError;
use crate::signals::{InterruptGate, spawn_force_exit_watchdog};

/// Default Opus bitrate in kbps (design 02).
const DEFAULT_BITRATE_KBPS: u32 = 64;
/// Default requested channel count (mono for microphone capture).
const DEFAULT_CHANNELS: u8 = 1;
/// Default requested channel count for system-audio capture (stereo mix).
const DEFAULT_SYSTEM_CHANNELS: u8 = 2;
/// Opus bitrate bounds enforced by `shiguredo_opus` (design 04).
const MIN_OPUS_BITRATE_BPS: u32 = 500;
const MAX_OPUS_BITRATE_BPS: u32 = 512_000;

/// Record audio (microphone or system output) to a file.
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct RecordArgs {
    /// Output path (`.wav` → WAV, `.ogg`/`.opus` → Ogg/Opus).
    #[usage(value_name = "OUTPUT")]
    output: Option<PathBuf>,

    /// Audio source: `mic` (default) or `system` (the system's output mix).
    #[usage(long, default = "mic")]
    source: Option<String>,

    /// Device to record from (`unique_id`, or a case-insensitive name match).
    /// Only used with `--source mic`.
    #[usage(long)]
    device: Option<String>,

    /// Requested channel count (1 or 2; the device's actual count is used).
    #[usage(long)]
    channels: Option<u8>,

    /// Opus bitrate in kbps (default 64; ignored for WAV).
    #[usage(long)]
    bitrate: Option<u32>,

    /// Stop automatically after this many seconds (e.g. `90` or `1.5`).
    #[usage(long)]
    duration: Option<f64>,

    /// Force the output format: `wav` or `ogg` (default: Ogg/Opus).
    #[usage(long)]
    format: Option<String>,

    /// Suppress the live progress line (logs stay on stderr).
    #[usage(long)]
    quiet: bool,
}

impl Run for RecordArgs {
    fn run(self) -> Result<(), MainError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| MainError::Internal(e.to_string()))?;
        runtime.block_on(self.record())
    }
}

impl RecordArgs {
    async fn record(self) -> Result<(), MainError> {
        let output = self.output.unwrap_or_else(default_output_path);
        let format = resolve_format(&output, self.format.as_deref())?;
        let source = resolve_source(self.source.as_deref())?;
        let device_id = match source {
            AudioSource::Microphone => resolve_device_id(self.device.as_deref())?,
            AudioSource::System => None,
        };
        let channels = self.channels.unwrap_or(if source == AudioSource::System {
            DEFAULT_SYSTEM_CHANNELS
        } else {
            DEFAULT_CHANNELS
        });
        if !matches!(channels, 1 | 2) {
            return Err(MainError::InvalidArgs(format!(
                "channels must be 1 or 2, got {channels}"
            )));
        }
        let bitrate_bps = self.bitrate.unwrap_or(DEFAULT_BITRATE_KBPS) * 1_000;
        if !(MIN_OPUS_BITRATE_BPS..=MAX_OPUS_BITRATE_BPS).contains(&bitrate_bps) {
            return Err(MainError::InvalidArgs(format!(
                "bitrate out of range ({MIN_OPUS_BITRATE_BPS}–{MAX_OPUS_BITRATE_BPS} bps)"
            )));
        }
        let duration = self.duration.map(Duration::from_secs_f64);
        let title = output.file_name().map_or_else(
            || output.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let tags = Tags {
            title,
            encoder: format!("oto v{}", env!("CARGO_PKG_VERSION")),
            created: jiff::Zoned::now().to_string(),
        };

        let config = RecordingConfig {
            output: output.clone(),
            format,
            source,
            device_id,
            channels,
            bitrate_bps: Some(bitrate_bps),
            tags,
        };
        let session =
            RecordingSession::start(&config).map_err(|e| to_main_error(e, &output, true))?;
        let spec = session.spec();

        if !self.quiet {
            let what = match source {
                AudioSource::Microphone => "microphone input",
                AudioSource::System => "system audio",
            };
            eprintln!(
                "Recording {what} to {} [{spec}] — Ctrl-C to stop",
                output.display()
            );
        }

        let gate = InterruptGate::new();
        wait_for_stop(duration).await;
        // The first interrupt is now consumed by `wait_for_stop`'s stream.
        // Register the force-exit watchdog *after* so it only sees a genuine
        // second tap during finalize (a single Ctrl-C must stop gracefully).
        gate.arm();
        spawn_force_exit_watchdog(gate.flag());

        let stats = session
            .stop()
            .map_err(|e| to_main_error(e, &output, false))?;
        if !self.quiet {
            eprintln!();
        }
        println!(
            "Wrote {} ({} KB, {} dropped)",
            output.display(),
            stats.bytes / 1024,
            stats.dropped
        );
        Ok(())
    }
}

/// Waits for SIGINT/SIGTERM (graceful) or the duration timer, whichever comes
/// first. Returns after the first interrupt is consumed, leaving the caller to
/// register the second-tap force-exit watchdog.
async fn wait_for_stop(duration: Option<Duration>) {
    let timer = async {
        match duration {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(timer);

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt()).ok();
        let mut sigterm = signal(SignalKind::terminate()).ok();

        tokio::select! {
            () = &mut timer, if duration.is_some() => {},
            () = await_signal(&mut sigint) => {},
            () = await_signal(&mut sigterm) => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            () = &mut timer, if duration.is_some() => {},
            () = tokio::signal::ctrl_c() => {},
        }
    }
}

/// Awaits a signal registration, parking forever if it failed to register or
/// its driver closed, so a missing signal can never be mistaken for a stop.
#[cfg(unix)]
async fn await_signal(sig: &mut Option<tokio::signal::unix::Signal>) {
    if let Some(sig) = sig
        && sig.recv().await.is_some()
    {
        return;
    }
    std::future::pending::<()>().await;
}

/// Resolves the output format from `--format`, falling back to the extension.
/// Unknown or absent extensions default to Ogg/Opus (design 02).
fn resolve_format(
    output: &Path,
    format: Option<&str>,
) -> Result<OutputFormat, MainError> {
    if let Some(f) = format {
        return match f.to_ascii_lowercase().as_str() {
            "wav" => Ok(OutputFormat::Wav),
            "ogg" | "opus" => Ok(OutputFormat::OggOpus),
            other => Err(MainError::InvalidArgs(format!("unknown format: {other}"))),
        };
    }
    match output
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(e) if e == "wav" => Ok(OutputFormat::Wav),
        _ => Ok(OutputFormat::OggOpus),
    }
}

/// Resolves the `--source` selector to an [`AudioSource`]. `mic` is the
/// default; `system` captures the system's output mix.
fn resolve_source(selector: Option<&str>) -> Result<AudioSource, MainError> {
    match selector.map(str::to_ascii_lowercase).as_deref() {
        None | Some("mic" | "microphone") => Ok(AudioSource::Microphone),
        Some("system" | "loopback") => Ok(AudioSource::System),
        Some(other) => Err(MainError::InvalidArgs(format!(
            "unknown source: {other} (expected `mic` or `system`)"
        ))),
    }
}

/// Resolves a `--device` selector to a `unique_id`: exact `unique_id` match
/// first, then a case-insensitive substring match on the name.
fn resolve_device_id(selector: Option<&str>) -> Result<Option<String>, MainError> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    let devices = list_input_devices().map_err(|e| MainError::Capture(e.to_string()))?;
    if let Some(device) = devices.iter().find(|d| d.unique_id == selector) {
        return Ok(Some(device.unique_id.clone()));
    }
    let needle = selector.to_lowercase();
    if let Some(device) = devices
        .iter()
        .find(|d| d.name.to_lowercase().contains(&needle))
    {
        return Ok(Some(device.unique_id.clone()));
    }
    Err(MainError::Capture(format!(
        "no input device matching '{selector}' (check microphone permissions and connections)"
    )))
}

/// Default output filename `oto-<timestamp>.ogg` in the current directory,
/// formatted as `oto-YYYYmmdd-HHMMSS.ogg` (design 02).
fn default_output_path() -> PathBuf {
    let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S");
    PathBuf::from(format!("oto-{stamp}.ogg"))
}

/// Maps a [`RecordingError`] to a [`MainError`], treating startup failures as
/// capture errors and finalize failures as I/O errors.
fn to_main_error(
    error: RecordingError,
    output: &Path,
    during_start: bool,
) -> MainError {
    match error {
        RecordingError::Capture(e) => MainError::Capture(e.to_string()),
        RecordingError::Encode(e) if during_start => MainError::Internal(e.to_string()),
        RecordingError::Encode(e) => MainError::Io(format!("{}: {e}", output.display())),
        RecordingError::Output(e) => MainError::Io(format!("{}: {e}", output.display())),
        RecordingError::ConsumerPanicked => {
            MainError::Internal("consumer thread panicked".to_owned())
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn format_from_extension() {
        assert_eq!(
            resolve_format(Path::new("a.ogg"), None).unwrap(),
            OutputFormat::OggOpus
        );
        assert_eq!(
            resolve_format(Path::new("a.opus"), None).unwrap(),
            OutputFormat::OggOpus
        );
        assert_eq!(
            resolve_format(Path::new("a.wav"), None).unwrap(),
            OutputFormat::Wav
        );
        // Unknown or absent extensions default to Ogg/Opus.
        assert_eq!(
            resolve_format(Path::new("a.txt"), None).unwrap(),
            OutputFormat::OggOpus
        );
        assert_eq!(
            resolve_format(Path::new("noext"), None).unwrap(),
            OutputFormat::OggOpus
        );
    }

    #[test]
    fn default_output_path_is_ogg() {
        let path = default_output_path();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("ogg"));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("oto-")
        );
    }

    #[test]
    fn explicit_format_wins_over_extension() {
        assert_eq!(
            resolve_format(Path::new("a.wav"), Some("ogg")).unwrap(),
            OutputFormat::OggOpus
        );
        assert_eq!(
            resolve_format(Path::new("a.ogg"), Some("wav")).unwrap(),
            OutputFormat::Wav
        );
        assert!(resolve_format(Path::new("a.ogg"), Some("mp3")).is_err());
    }

    #[test]
    fn resolves_source_selector() {
        assert_eq!(resolve_source(None).unwrap(), AudioSource::Microphone);
        assert_eq!(
            resolve_source(Some("mic")).unwrap(),
            AudioSource::Microphone
        );
        assert_eq!(
            resolve_source(Some("MIC")).unwrap(),
            AudioSource::Microphone
        );
        assert_eq!(resolve_source(Some("system")).unwrap(), AudioSource::System);
        assert_eq!(
            resolve_source(Some("loopback")).unwrap(),
            AudioSource::System
        );
        assert!(resolve_source(Some("camera")).is_err());
    }
}
