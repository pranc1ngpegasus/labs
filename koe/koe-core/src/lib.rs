//! koe-core — shared pipeline, AEC, codecs, and state.

pub(crate) mod aec;
pub(crate) mod codec;
pub(crate) mod pipeline;
pub(crate) mod transcript;

pub use aec::{AcousticEchoCanceller, AecConfig};
pub use pipeline::{
    PipelineConfig, PipelineError, PipelineMetricsSnapshot, RecordingPipeline, RecordingState,
    RecordingStatus, StopResult, available_disk_space,
};

/// Error returned by [`available_disk_space`] and recording setup.
pub use koe_ffi::RecordingError;
/// Summary payload returned from [`RecordingPipeline::stop`].
pub use koe_ffi::RecordingSummary;

/// Discovery, permission, and transcription entry points used by `koe-cli` (and GUI).
pub use koe_ffi::{
    AppInfo, AudioDeviceInfo, AudioSourceConfig, OutputFormat, Permission, PermissionStatus,
    SpeechEngine, TranscriptFormat, TranscriptionCallback, TranscriptionError, TranscriptionHandle,
    TranscriptionSegment, check_permission, default_input_device, default_output_device,
    enumerate_apps, feed_transcription_audio, finalize_transcription,
    install_default_native_provider, native_provider_registered, start_transcription,
    supported_speech_locales, validate_locale,
};

/// Transcript formatters and path helpers for CLI/GUI output.
pub use transcript::{
    TranscriptFormatter, TranscriptMeta, create_formatter, default_transcript_path,
    transcript_extension,
};

/// Compile-time feature flags enabled in this `koe-core` build.
#[must_use]
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "aec") {
        features.push("aec");
    }
    if cfg!(feature = "cli") {
        features.push("cli");
    }
    if cfg!(feature = "gui") {
        features.push("gui");
    }
    if cfg!(feature = "ogg") {
        features.push("ogg");
    }
    if cfg!(feature = "screen-audio") {
        features.push("screen-audio");
    }
    if cfg!(feature = "system-audio") {
        features.push("system-audio");
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_include_cli_stack() {
        let features = enabled_features();
        assert!(features.contains(&"aec"));
        assert!(features.contains(&"cli"));
        assert!(features.contains(&"ogg"));
        assert!(features.contains(&"screen-audio"));
        assert!(features.contains(&"system-audio"));
        assert!(!features.contains(&"gui"));
    }
}
