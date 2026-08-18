//! Opaque session handles exported across the FFI boundary.

#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::callbacks::{AudioCallbackRef, ProgressCallbackRef, TranscriptionCallbackRef};
use crate::types::{AudioSourceConfig, RecordingStatus, TranscriptionSegment};

#[cfg(target_os = "macos")]
use crate::macos_capture::CaptureSession;

static NEXT_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

fn next_handle_id() -> u64 {
    NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Active audio capture session.
#[derive(uniffi::Object)]
pub struct CaptureHandle {
    #[expect(dead_code)]
    pub(crate) id: u64,
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub(crate) source: AudioSourceConfig,
    pub(crate) callback: AudioCallbackRef,
    #[cfg(target_os = "macos")]
    session: Mutex<Option<Box<dyn CaptureSession>>>,
}

impl CaptureHandle {
    // Constructed from `start_capture` on macOS; tests exercise it on all targets.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn new(
        source: AudioSourceConfig,
        callback: AudioCallbackRef,
    ) -> Self {
        Self {
            id: next_handle_id(),
            source,
            callback,
            #[cfg(target_os = "macos")]
            session: Mutex::new(None),
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn attach_session(
        &self,
        session: Box<dyn CaptureSession>,
    ) {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(session);
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn stop_session(&self) {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut session) = guard.take() {
            session.stop();
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[expect(clippy::unused_self)]
    pub(crate) const fn stop_session(&self) {}
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop_session();
    }
}

#[uniffi::export]
impl CaptureHandle {
    /// Forwards a PCM chunk to the registered [`crate::AudioCallback`].
    ///
    /// Called by the native capture bridge. Must stay non-blocking: the
    /// callback implementation should only enqueue work.
    pub fn deliver_audio(
        &self,
        pcm: Vec<f32>,
        timestamp_ms: u64,
    ) {
        self.callback.on_audio(pcm, timestamp_ms);
    }
}

/// Active speech transcription session.
#[derive(uniffi::Object)]
pub struct TranscriptionHandle {
    #[expect(dead_code)]
    pub(crate) id: u64,
    #[cfg_attr(not(target_os = "macos"), expect(dead_code))]
    pub(crate) locale: String,
    pub(crate) callback: TranscriptionCallbackRef,
    /// Native recognition session, when one is running (macOS only).
    #[cfg(target_os = "macos")]
    session: Mutex<Option<crate::speech_session::SpeechSession>>,
}

impl TranscriptionHandle {
    pub(crate) fn new(
        locale: String,
        callback: TranscriptionCallbackRef,
    ) -> Self {
        Self {
            id: next_handle_id(),
            locale,
            callback,
            #[cfg(target_os = "macos")]
            session: Mutex::new(None),
        }
    }

    /// Attaches the native recognition session started by
    /// [`crate::start_transcription`].
    #[cfg(target_os = "macos")]
    pub(crate) fn attach_session(
        &self,
        session: crate::speech_session::SpeechSession,
    ) {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(session);
    }

    /// Takes (or leaves) the native recognition session for feeding/finalize.
    #[cfg(target_os = "macos")]
    pub(crate) fn with_session<R>(
        &self,
        f: impl FnOnce(Option<&mut crate::speech_session::SpeechSession>) -> R,
    ) -> R {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(guard.as_mut())
    }
}

#[uniffi::export]
impl TranscriptionHandle {
    /// Forwards a transcript segment to the registered callback.
    pub fn deliver_segment(
        &self,
        segment: TranscriptionSegment,
    ) {
        self.callback.on_segment(segment);
    }

    /// Forwards a transcription failure to the registered callback.
    pub fn deliver_error(
        &self,
        error: String,
    ) {
        self.callback.on_error(error);
    }
}

/// Active audio-monitoring (pass-through output) session.
#[derive(uniffi::Object)]
pub struct MonitorHandle {
    pub(crate) id: u64,
}

impl MonitorHandle {
    pub(crate) fn new() -> Self {
        Self {
            id: next_handle_id(),
        }
    }
}

#[uniffi::export]
impl MonitorHandle {
    /// Stable session id for native/debug correlation.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }
}

/// Active recording session spanning capture, encoding, and transcription.
#[derive(uniffi::Object)]
pub struct RecordingHandle {
    pub(crate) id: u64,
    pub(crate) source: AudioSourceConfig,
    pub(crate) output_path: String,
    pub(crate) locale: String,
    pub(crate) progress_callback: ProgressCallbackRef,
}

impl RecordingHandle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: AudioSourceConfig,
        output_path: String,
        locale: String,
        progress_callback: ProgressCallbackRef,
    ) -> Self {
        Self {
            id: next_handle_id(),
            source,
            output_path,
            locale,
            progress_callback,
        }
    }
}

#[uniffi::export]
impl RecordingHandle {
    /// Forwards a progress status update to the registered callback.
    pub fn deliver_status(
        &self,
        status: RecordingStatus,
    ) {
        self.progress_callback.on_status(status);
    }

    /// Forwards a live transcript segment to the registered callback.
    pub fn deliver_segment(
        &self,
        segment: TranscriptionSegment,
    ) {
        self.progress_callback.on_segment(segment);
    }

    /// Forwards a recording-session error to the registered callback.
    pub fn deliver_error(
        &self,
        error: String,
    ) {
        self.progress_callback.on_error(error);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::callbacks::{AudioCallback, ProgressCallback, TranscriptionCallback};
    use crate::types::RecordingState;

    type AudioCalls = Arc<Mutex<Vec<(Vec<f32>, u64)>>>;
    type SegmentLog = Arc<Mutex<Vec<TranscriptionSegment>>>;
    type StatusLog = Arc<Mutex<Vec<RecordingStatus>>>;
    type ErrorLog = Arc<Mutex<Vec<String>>>;

    struct MockAudio {
        calls: AudioCalls,
    }

    impl AudioCallback for MockAudio {
        fn on_audio(
            &self,
            pcm: Vec<f32>,
            timestamp_ms: u64,
        ) {
            self.calls.lock().expect("lock").push((pcm, timestamp_ms));
        }
    }

    struct MockTranscription {
        segments: SegmentLog,
        errors: ErrorLog,
    }

    impl TranscriptionCallback for MockTranscription {
        fn on_segment(
            &self,
            segment: TranscriptionSegment,
        ) {
            self.segments.lock().expect("lock").push(segment);
        }

        fn on_error(
            &self,
            error: String,
        ) {
            self.errors.lock().expect("lock").push(error);
        }
    }

    struct MockProgress {
        statuses: StatusLog,
        errors: ErrorLog,
    }

    impl ProgressCallback for MockProgress {
        fn on_status(
            &self,
            status: RecordingStatus,
        ) {
            self.statuses.lock().expect("lock").push(status);
        }

        fn on_segment(
            &self,
            _segment: TranscriptionSegment,
        ) {
        }

        fn on_error(
            &self,
            error: String,
        ) {
            self.errors.lock().expect("lock").push(error);
        }
    }

    #[test]
    fn deliver_audio_preserves_pcm_and_monotonic_timestamps() {
        let calls: AudioCalls = Arc::new(Mutex::new(Vec::new()));
        let handle = CaptureHandle::new(
            AudioSourceConfig::Microphone,
            Box::new(MockAudio {
                calls: Arc::clone(&calls),
            }),
        );

        handle.deliver_audio(vec![0.1, -0.1], 10);
        handle.deliver_audio(vec![0.2, -0.2], 20);

        let recorded = calls.lock().expect("lock").clone();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, vec![0.1, -0.1]);
        assert_eq!(recorded[0].1, 10);
        assert_eq!(recorded[1].0, vec![0.2, -0.2]);
        assert_eq!(recorded[1].1, 20);
        assert!(recorded[0].1 < recorded[1].1);
    }

    #[test]
    fn deliver_transcription_segment_and_error() {
        let segments: SegmentLog = Arc::new(Mutex::new(Vec::new()));
        let errors: ErrorLog = Arc::new(Mutex::new(Vec::new()));
        let handle = TranscriptionHandle::new(
            "en-US".into(),
            Box::new(MockTranscription {
                segments: Arc::clone(&segments),
                errors: Arc::clone(&errors),
            }),
        );

        handle.deliver_segment(TranscriptionSegment {
            text: "hello".into(),
            start_ms: 0,
            end_ms: 100,
            is_final: true,
            confidence: 0.9,
        });
        handle.deliver_error("boom".into());

        assert_eq!(segments.lock().expect("lock").len(), 1);
        assert_eq!(errors.lock().expect("lock").as_slice(), ["boom"]);
    }

    #[test]
    fn deliver_progress_status_and_error() {
        let statuses: StatusLog = Arc::new(Mutex::new(Vec::new()));
        let errors: ErrorLog = Arc::new(Mutex::new(Vec::new()));
        let handle = RecordingHandle::new(
            AudioSourceConfig::Microphone,
            "/tmp/out.ogg".into(),
            "en-US".into(),
            Box::new(MockProgress {
                statuses: Arc::clone(&statuses),
                errors: Arc::clone(&errors),
            }),
        );

        handle.deliver_status(RecordingStatus {
            elapsed_ms: 42,
            bytes_written: 100,
            level_left: 0.1,
            level_right: 0.2,
            state: RecordingState::Recording,
        });
        handle.deliver_error("disk".into());

        assert_eq!(statuses.lock().expect("lock").len(), 1);
        assert_eq!(errors.lock().expect("lock").as_slice(), ["disk"]);
    }

    #[test]
    fn monitor_handle_ids_are_unique() {
        let a = MonitorHandle::new();
        let b = MonitorHandle::new();
        assert_ne!(a.id(), b.id());
    }
}
