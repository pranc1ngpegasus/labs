//! Callback traits bridging native capture/transcription and Rust consumers.

use crate::types::{RecordingStatus, TranscriptionSegment};

/// Receives PCM chunks from native audio capture.
#[uniffi::export(callback_interface)]
pub trait AudioCallback: Send + Sync {
    /// Called on the native capture thread.
    ///
    /// `pcm` is Float32, 48 kHz, interleaved stereo.
    /// `timestamp_ms` is a monotonic clock value for alignment.
    fn on_audio(
        &self,
        pcm: Vec<f32>,
        timestamp_ms: u64,
    );
}

/// Receives transcription segments from the speech analyzer bridge.
#[uniffi::export(callback_interface)]
pub trait TranscriptionCallback: Send + Sync {
    fn on_segment(
        &self,
        segment: TranscriptionSegment,
    );
    fn on_error(
        &self,
        error: String,
    );
}

/// Receives recording progress updates for CLI/GUI surfaces.
#[uniffi::export(callback_interface)]
pub trait ProgressCallback: Send + Sync {
    fn on_status(
        &self,
        status: RecordingStatus,
    );
    fn on_segment(
        &self,
        segment: TranscriptionSegment,
    );
    fn on_error(
        &self,
        error: String,
    );
}

pub type AudioCallbackRef = Box<dyn AudioCallback>;
pub type TranscriptionCallbackRef = Box<dyn TranscriptionCallback>;
pub type ProgressCallbackRef = Box<dyn ProgressCallback>;
