//! koe-ffi — uniffi-generated bindings and type conversions.

mod api;
mod callbacks;
mod error;
mod handles;
mod native;
mod types;

#[cfg(target_os = "macos")]
mod macos_capture;
#[cfg(target_os = "macos")]
mod macos_discovery;
#[cfg(target_os = "macos")]
mod macos_system;

pub use api::{
    check_permission, enumerate_apps, feed_monitor, feed_transcription_audio,
    finalize_transcription, pause_recording, request_permission, resume_recording,
    set_capture_stub, set_transcription_stub, start_capture, start_monitor, start_recording,
    start_transcription, stop_capture, stop_monitor, stop_recording,
};
pub use callbacks::{
    AudioCallback, AudioCallbackRef, ProgressCallback, ProgressCallbackRef, TranscriptionCallback,
    TranscriptionCallbackRef,
};
pub use error::{
    CaptureError, MonitorError, RecordingError, RecordingSummary, TranscriptionError,
    validate_capture_source, validate_locale, validate_output_path,
};
pub use handles::{CaptureHandle, MonitorHandle, RecordingHandle, TranscriptionHandle};
pub use native::{NativeProvider, native_provider_registered, register_native_provider};
pub use types::{
    AppInfo, AudioDeviceInfo, AudioSourceConfig, OutputFormat, Permission, PermissionStatus,
    RecordingState, RecordingStatus, SpeechEngine, TranscriptFormat, TranscriptionSegment,
};

#[cfg(target_os = "macos")]
pub use macos_discovery::install_default_native_provider;
#[cfg(target_os = "macos")]
pub use macos_system::{default_input_device, default_output_device, supported_speech_locales};

/// No-op on non-macOS targets.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn install_default_native_provider() -> bool {
    false
}

/// No default input device outside macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn default_input_device() -> Option<AudioDeviceInfo> {
    None
}

/// No default output device outside macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn default_output_device() -> Option<AudioDeviceInfo> {
    None
}

/// Speech locale enumeration requires the Speech framework (macOS only).
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn supported_speech_locales() -> Vec<String> {
    Vec::new()
}

uniffi::setup_scaffolding!();

/// Smoke-test export used to verify uniffi Swift binding generation.
#[uniffi::export]
#[must_use]
pub const fn add(
    left: u64,
    right: u64,
) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn add_works() {
        assert_eq!(add(2, 2), 4);
    }

    #[test]
    fn permission_defaults_without_provider() {
        assert_eq!(
            check_permission(Permission::Microphone),
            PermissionStatus::NotDetermined
        );
    }

    #[test]
    fn enumerate_apps_empty_without_provider() {
        assert!(enumerate_apps().is_empty());
    }

    #[test]
    fn native_provider_starts_unregistered() {
        // Other tests in this crate do not register a provider.
        assert!(!native_provider_registered());
    }

    #[test]
    fn monitor_start_feed_stop_round_trip() {
        let handle = start_monitor().expect("start");
        feed_monitor(Arc::clone(&handle), vec![0.1, -0.1, 0.2, -0.2]).expect("feed");
        stop_monitor(handle);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn system_queries_degrade_without_macos() {
        assert!(default_input_device().is_none());
        assert!(default_output_device().is_none());
        assert!(supported_speech_locales().is_empty());
    }
}
