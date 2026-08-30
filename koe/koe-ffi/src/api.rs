//! Exported FFI entry points.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::callbacks::{AudioCallbackRef, ProgressCallbackRef, TranscriptionCallbackRef};
use crate::error::{
    CaptureError, MonitorError, RecordingError, RecordingSummary, TranscriptionError,
    validate_capture_source, validate_locale, validate_output_path,
};
use crate::handles::{CaptureHandle, MonitorHandle, RecordingHandle, TranscriptionHandle};
use crate::native;
#[cfg(target_os = "macos")]
use crate::types::TranscriptionSegment;
use crate::types::{
    AppInfo, AudioSourceConfig, OutputFormat, Permission, PermissionStatus, SpeechEngine,
};

#[must_use]
#[uniffi::export]
pub fn check_permission(permission: Permission) -> PermissionStatus {
    native::provider().map_or(PermissionStatus::NotDetermined, |provider| {
        provider.check_permission(permission)
    })
}

#[must_use]
#[uniffi::export]
pub fn request_permission(permission: Permission) -> PermissionStatus {
    native::provider().map_or(PermissionStatus::NotDetermined, |provider| {
        provider.request_permission(permission)
    })
}

#[must_use]
#[uniffi::export]
pub fn enumerate_apps() -> Vec<AppInfo> {
    native::provider().map_or_else(Vec::new, |provider| provider.enumerate_apps())
}

#[allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn start_capture(
    source: AudioSourceConfig,
    callback: AudioCallbackRef,
) -> Result<Arc<CaptureHandle>, CaptureError> {
    validate_capture_source(&source)?;
    #[cfg(not(target_os = "macos"))]
    {
        if !capture_stubbed() {
            return Err(CaptureError::Internal {
                msg: "audio capture is only available on macOS".to_owned(),
            });
        }
        Ok(Arc::new(CaptureHandle::new(source, callback)))
    }
    #[cfg(target_os = "macos")]
    {
        let handle = Arc::new(CaptureHandle::new(source, callback));
        if !capture_stubbed() {
            let session = crate::macos_capture::start_session(Arc::clone(&handle))?;
            handle.attach_session(session);
        }
        Ok(handle)
    }
}

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn stop_capture(handle: Arc<CaptureHandle>) {
    handle.stop_session();
}

/// Starts a monitoring session that plays clean PCM to the default output.
///
/// On macOS this backs onto `oto-capture`'s `AudioPlayback` (Shiguredo); on
/// other platforms it allocates an inert handle so `koe-core` can exercise the
/// start/feed/stop path on all targets.
#[allow(clippy::missing_errors_doc)]
#[uniffi::export]
pub fn start_monitor() -> Result<Arc<MonitorHandle>, MonitorError> {
    let handle = Arc::new(MonitorHandle::new());
    #[cfg(target_os = "macos")]
    {
        let session = oto_capture::PlaybackSession::start()
            .map_err(|e| MonitorError::CreateFailed { msg: e.to_string() })?;
        handle.attach_session(session);
    }
    Ok(handle)
}

/// Enqueues interleaved stereo Float32 PCM for monitoring playback.
///
/// # Errors
///
/// Returns [`MonitorError::NotRunning`] when the session has already been
/// stopped. Native bridges may also return [`MonitorError::Internal`].
#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn feed_monitor(
    handle: Arc<MonitorHandle>,
    pcm: Vec<f32>,
) -> Result<(), MonitorError> {
    #[cfg(target_os = "macos")]
    {
        handle.feed(&pcm)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (handle, pcm);
        Ok(())
    }
}

/// Stops monitoring and releases the output session.
#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn stop_monitor(handle: Arc<MonitorHandle>) {
    #[cfg(target_os = "macos")]
    {
        handle.stop_session();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = handle;
    }
}

/// When true, [`start_capture`] returns a handle without a native session
/// (test / CI harness only).
static STUB_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Enables or disables no-op capture for tests that only need lifecycle.
///
/// Not part of the supported public API — test / CI harness only.
#[doc(hidden)]
pub fn set_capture_stub(enabled: bool) {
    STUB_CAPTURE.store(enabled, Ordering::SeqCst);
}

fn capture_stubbed() -> bool {
    STUB_CAPTURE.load(Ordering::SeqCst) || std::env::var_os("KOE_STUB_CAPTURE").is_some()
}

/// When true, [`start_transcription`] returns a handle without a native speech
/// session (test / CI harness only — mirrors [`crate::set_capture_stub`]).
static STUB_TRANSCRIPTION: AtomicBool = AtomicBool::new(false);

/// Enables or disables no-op transcription for tests that only need lifecycle.
///
/// Not part of the supported public API — test / CI harness only.
#[doc(hidden)]
pub fn set_transcription_stub(enabled: bool) {
    STUB_TRANSCRIPTION.store(enabled, Ordering::SeqCst);
}

#[cfg(target_os = "macos")]
fn transcription_stubbed() -> bool {
    STUB_TRANSCRIPTION.load(Ordering::SeqCst)
}

/// Maps a koe-transcribe error onto the exported `UniFFI` error type.
#[cfg(target_os = "macos")]
fn map_transcription_error(error: koe_transcribe::Error) -> TranscriptionError {
    match error {
        koe_transcribe::Error::PermissionDenied(msg) => {
            TranscriptionError::PermissionDenied { msg }
        },
        koe_transcribe::Error::UnsupportedLocale(locale) => {
            TranscriptionError::UnsupportedLocale { locale }
        },
        koe_transcribe::Error::NotAvailable => TranscriptionError::NotAvailable,
        koe_transcribe::Error::OnDeviceUnavailable { msg } => {
            TranscriptionError::OnDeviceUnavailable { msg }
        },
        koe_transcribe::Error::Internal(msg) => TranscriptionError::Internal { msg },
    }
}

#[cfg(target_os = "macos")]
const fn to_requested_engine(engine: SpeechEngine) -> koe_transcribe::RequestedEngine {
    match engine {
        SpeechEngine::Auto => koe_transcribe::RequestedEngine::Auto,
        SpeechEngine::OnDevice => koe_transcribe::RequestedEngine::OnDevice,
        SpeechEngine::Network => koe_transcribe::RequestedEngine::Network,
    }
}

#[cfg(target_os = "macos")]
fn start_transcription_native(
    handle: &Arc<TranscriptionHandle>,
    engine: SpeechEngine,
) -> Result<(), TranscriptionError> {
    if transcription_stubbed() {
        return Ok(());
    }
    let session = start_koe_session(handle, engine)?;
    if engine == SpeechEngine::Auto && session.engine() == koe_transcribe::Engine::Network {
        eprintln!(
            "warning: on-device speech recognition is unavailable (Siri & Dictation \
             disabled); falling back to network recognition — audio is sent to Apple's servers"
        );
    }
    handle.attach_session(session);
    Ok(())
}

/// Starts a [`SpeechAnalyzer`](koe_transcribe::SpeechAnalyzer) wired to
/// `handle`'s segment/error callbacks, mapping the engine and errors onto the
/// exported API surface.
#[cfg(target_os = "macos")]
fn start_koe_session(
    handle: &Arc<TranscriptionHandle>,
    engine: SpeechEngine,
) -> Result<koe_transcribe::SpeechAnalyzer, TranscriptionError> {
    let weak = Arc::downgrade(handle);
    let on_segment = move |segment: &koe_transcribe::Segment| {
        let Some(handle) = weak.upgrade() else {
            return;
        };
        handle.deliver_segment(TranscriptionSegment {
            text: segment.text.clone(),
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            is_final: segment.is_final,
            confidence: segment.confidence,
        });
    };
    let weak = Arc::downgrade(handle);
    let on_error = move |message: &str| {
        let Some(handle) = weak.upgrade() else {
            return;
        };
        handle.deliver_error(message.to_owned());
    };
    koe_transcribe::SpeechAnalyzer::start(
        &handle.locale,
        to_requested_engine(engine),
        on_segment,
        on_error,
    )
    .map_err(map_transcription_error)
}

#[allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn start_transcription(
    locale: String,
    engine: SpeechEngine,
    callback: TranscriptionCallbackRef,
) -> Result<Arc<TranscriptionHandle>, TranscriptionError> {
    validate_locale(&locale)?;
    let handle = Arc::new(TranscriptionHandle::new(locale, callback));
    #[cfg(target_os = "macos")]
    {
        start_transcription_native(&handle, engine)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = engine;
    Ok(handle)
}

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn feed_transcription_audio(
    handle: Arc<TranscriptionHandle>,
    pcm: Vec<f32>,
) {
    #[cfg(target_os = "macos")]
    {
        handle.with_session(|session| {
            if let Some(session) = session {
                session.feed(&pcm);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    drop((handle, pcm));
}

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn finalize_transcription(handle: Arc<TranscriptionHandle>) {
    #[cfg(target_os = "macos")]
    {
        // Take the session out of the handle so the blocking finalize wait
        // never holds the handle lock (feeding may still be in flight from a
        // capture thread).
        let Some(mut session) = handle.take_session() else {
            // No active session (already finalized or never started).
            eprintln!("warning: transcription session not active at finalize");
            return;
        };
        if let Err(err) = session.finish() {
            eprintln!("warning: transcription finalize failed: {err}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    drop(handle);
}

#[allow(clippy::too_many_arguments, clippy::missing_errors_doc)]
#[uniffi::export]
pub fn start_recording(
    source: AudioSourceConfig,
    output_path: String,
    locale: String,
    format: OutputFormat,
    enable_aec: bool,
    comfort_noise: bool,
    progress_callback: ProgressCallbackRef,
) -> Result<Arc<RecordingHandle>, RecordingError> {
    let _ = (format, enable_aec, comfort_noise);
    validate_capture_source(&source)?;
    validate_locale(&locale)?;
    validate_output_path(&output_path)?;
    Ok(Arc::new(RecordingHandle::new(
        source,
        output_path,
        locale,
        progress_callback,
    )))
}

#[allow(clippy::needless_pass_by_value, clippy::missing_errors_doc)]
#[uniffi::export]
pub fn stop_recording(handle: Arc<RecordingHandle>) -> Result<RecordingSummary, RecordingError> {
    let _ = (
        &handle.source,
        &handle.output_path,
        &handle.locale,
        &handle.progress_callback,
        handle.id,
    );
    Ok(RecordingSummary {
        duration_sec: 0.0,
        bytes_written: 0,
        transcript_segment_count: 0,
        dropped_audio_frames: 0,
        format: OutputFormat::Ogg { bitrate_bps: None },
    })
}

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn pause_recording(handle: Arc<RecordingHandle>) {
    let _ = (
        &handle.source,
        &handle.output_path,
        &handle.locale,
        &handle.progress_callback,
        handle.id,
    );
}

#[allow(clippy::needless_pass_by_value)]
#[uniffi::export]
pub fn resume_recording(handle: Arc<RecordingHandle>) {
    let _ = (
        &handle.source,
        &handle.output_path,
        &handle.locale,
        &handle.progress_callback,
        handle.id,
    );
}
