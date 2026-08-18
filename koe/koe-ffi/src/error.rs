//! Error types that cross the FFI boundary.

use crate::types::{AudioSourceConfig, OutputFormat};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CaptureError {
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("No audio source found for {bundle_id}")]
    NoAudioSource { bundle_id: String },
    #[error("Capture stream error: {msg}")]
    StreamError { msg: String },
    #[error("Internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TranscriptionError {
    #[error("Unsupported locale: {locale}")]
    UnsupportedLocale { locale: String },
    #[error("Analyzer not available on this OS version")]
    NotAvailable,
    #[error("On-device recognition unavailable: {msg}")]
    OnDeviceUnavailable { msg: String },
    #[error("Permission denied: {msg}")]
    PermissionDenied { msg: String },
    #[error("Transcription internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MonitorError {
    #[error("Failed to create audio monitor: {msg}")]
    CreateFailed { msg: String },
    #[error("Monitor is not running")]
    NotRunning,
    #[error("Monitor internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RecordingError {
    #[error("{0}")]
    Capture(#[from] CaptureError),
    #[error("{0}")]
    Transcription(#[from] TranscriptionError),
    #[error("Insufficient disk space: need {needed}, have {available}")]
    InsufficientDiskSpace { needed: u64, available: u64 },
    #[error("Output already exists: {path}")]
    OutputExists { path: String },
    #[error("Config validation error: {msg}")]
    ConfigError { msg: String },
    #[error("Internal error: {msg}")]
    Internal { msg: String },
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RecordingSummary {
    pub duration_sec: f64,
    pub bytes_written: u64,
    pub transcript_segment_count: u64,
    pub dropped_audio_frames: u64,
    pub format: OutputFormat,
}

/// Rejects capture sources that cannot produce audio.
///
/// # Errors
///
/// Returns [`CaptureError`] when the source configuration is invalid.
pub fn validate_capture_source(source: &AudioSourceConfig) -> Result<(), CaptureError> {
    match source {
        AudioSourceConfig::AppAudio { bundle_id } | AudioSourceConfig::Both { bundle_id }
            if bundle_id.trim().is_empty() =>
        {
            Err(CaptureError::NoAudioSource {
                bundle_id: bundle_id.clone(),
            })
        },
        AudioSourceConfig::PidAudio { pid } if *pid <= 0 => Err(CaptureError::Internal {
            msg: format!("invalid pid: {pid}"),
        }),
        _ => Ok(()),
    }
}

/// Rejects empty or whitespace-only locale tags.
///
/// # Errors
///
/// Returns [`TranscriptionError::UnsupportedLocale`] for empty locales.
pub fn validate_locale(locale: &str) -> Result<(), TranscriptionError> {
    if locale.trim().is_empty() {
        return Err(TranscriptionError::UnsupportedLocale {
            locale: locale.to_owned(),
        });
    }
    Ok(())
}

/// Rejects empty recording output paths.
///
/// # Errors
///
/// Returns [`RecordingError::ConfigError`] when the path is empty.
pub fn validate_output_path(path: &str) -> Result<(), RecordingError> {
    if path.trim().is_empty() {
        return Err(RecordingError::ConfigError {
            msg: "output path is empty".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_error_display_is_distinct() {
        assert_eq!(
            CaptureError::PermissionDenied("mic".into()).to_string(),
            "Permission denied: mic"
        );
        assert_eq!(
            CaptureError::NoAudioSource {
                bundle_id: "com.app".into()
            }
            .to_string(),
            "No audio source found for com.app"
        );
        assert_eq!(
            CaptureError::StreamError {
                msg: "underrun".into()
            }
            .to_string(),
            "Capture stream error: underrun"
        );
        assert_eq!(
            CaptureError::Internal { msg: "boom".into() }.to_string(),
            "Internal error: boom"
        );
    }

    #[test]
    fn transcription_error_display_is_distinct() {
        assert_eq!(
            TranscriptionError::UnsupportedLocale {
                locale: "xx".into()
            }
            .to_string(),
            "Unsupported locale: xx"
        );
        assert_eq!(
            TranscriptionError::NotAvailable.to_string(),
            "Analyzer not available on this OS version"
        );
        assert_eq!(
            TranscriptionError::OnDeviceUnavailable {
                msg: "enable dictation".into()
            }
            .to_string(),
            "On-device recognition unavailable: enable dictation"
        );
        assert_eq!(
            TranscriptionError::PermissionDenied {
                msg: "speech".into()
            }
            .to_string(),
            "Permission denied: speech"
        );
        assert_eq!(
            TranscriptionError::Internal { msg: "asr".into() }.to_string(),
            "Transcription internal error: asr"
        );
    }

    #[test]
    fn recording_error_from_nested_errors() {
        let from_capture: RecordingError = CaptureError::PermissionDenied("screen".into()).into();
        assert!(matches!(
            from_capture,
            RecordingError::Capture(CaptureError::PermissionDenied(_))
        ));

        let from_transcription: RecordingError = TranscriptionError::NotAvailable.into();
        assert!(matches!(
            from_transcription,
            RecordingError::Transcription(TranscriptionError::NotAvailable)
        ));
    }

    #[test]
    fn validate_capture_source_rejects_empty_bundle() {
        let err = validate_capture_source(&AudioSourceConfig::AppAudio {
            bundle_id: "  ".into(),
        })
        .unwrap_err();
        assert!(matches!(err, CaptureError::NoAudioSource { .. }));
    }

    #[test]
    fn validate_locale_rejects_empty() {
        let err = validate_locale("").unwrap_err();
        assert!(matches!(err, TranscriptionError::UnsupportedLocale { .. }));
    }

    #[test]
    fn validate_output_path_rejects_empty() {
        let err = validate_output_path("").unwrap_err();
        assert!(matches!(err, RecordingError::ConfigError { .. }));
    }

    #[test]
    fn monitor_error_display_is_distinct() {
        assert_eq!(
            MonitorError::CreateFailed {
                msg: "queue".into()
            }
            .to_string(),
            "Failed to create audio monitor: queue"
        );
        assert_eq!(
            MonitorError::NotRunning.to_string(),
            "Monitor is not running"
        );
        assert_eq!(
            MonitorError::Internal { msg: "x".into() }.to_string(),
            "Monitor internal error: x"
        );
    }
}
