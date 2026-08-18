//! CLI subcommands.
//!
//! `completions` waits on later tasks.

mod apps_table;
mod decode;
mod duration;
mod info;
mod list;
mod permissions;
mod record;
mod transcribe;

pub use info::InfoArgs;
pub use list::ListArgs;
pub use permissions::PermissionsArgs;
pub use record::RecordArgs;
pub use transcribe::TranscribeArgs;

use crate::MainError;
use crate::config::KoeConfig;

pub trait Run {
    fn run(
        self,
        config: &KoeConfig,
    ) -> Result<(), MainError>;
}

/// Parses a `--engine` value (`auto` / `on-device` / `network`) into the FFI
/// [`koe_core::SpeechEngine`]. Accepts `ondevice` as an alias for `on-device`.
fn parse_speech_engine(value: &str) -> Result<koe_core::SpeechEngine, MainError> {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "auto" => Ok(koe_core::SpeechEngine::Auto),
        "on-device" | "ondevice" | "local" => Ok(koe_core::SpeechEngine::OnDevice),
        "network" | "server" | "cloud" => Ok(koe_core::SpeechEngine::Network),
        other => Err(MainError::InvalidArgs(format!(
            "unknown --engine '{other}' (expected auto, on-device, or network)"
        ))),
    }
}

fn parse_transcript_format(value: &str) -> Result<koe_core::TranscriptFormat, MainError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "txt" => Ok(koe_core::TranscriptFormat::Txt),
        "srt" => Ok(koe_core::TranscriptFormat::Srt),
        "vtt" => Ok(koe_core::TranscriptFormat::Vtt),
        "json" => Ok(koe_core::TranscriptFormat::Json),
        other => Err(MainError::InvalidArgs(format!(
            "unknown transcript format '{other}' (expected txt, srt, vtt, or json)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_speech_engine_variants() {
        assert_eq!(
            parse_speech_engine("auto").unwrap(),
            koe_core::SpeechEngine::Auto
        );
        assert_eq!(
            parse_speech_engine("on_device").unwrap(),
            koe_core::SpeechEngine::OnDevice
        );
        assert_eq!(
            parse_speech_engine("local").unwrap(),
            koe_core::SpeechEngine::OnDevice
        );
        assert_eq!(
            parse_speech_engine("server").unwrap(),
            koe_core::SpeechEngine::Network
        );
        assert!(parse_speech_engine("banana").is_err());
    }

    #[test]
    fn parses_transcript_format_variants() {
        assert_eq!(
            parse_transcript_format("JSON").unwrap(),
            koe_core::TranscriptFormat::Json
        );
        assert_eq!(
            parse_transcript_format("srt").unwrap(),
            koe_core::TranscriptFormat::Srt
        );
        assert!(parse_transcript_format("docx").is_err());
    }
}
