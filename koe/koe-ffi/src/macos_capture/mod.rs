//! macOS audio capture sessions for [`crate::start_capture`].
//!
//! Implements Process Tap (`PidAudio`), ScreenCaptureKit (`AppAudio`), and
//! AudioQueue microphone capture without linking the Swift `koe-native` dylib.

#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::redundant_pub_crate,
    clippy::struct_field_names,
    clippy::unwrap_used
)]

mod microphone;
mod process_tap;
mod screen_audio;
mod timestamp;

use std::sync::Arc;

use crate::error::CaptureError;
use crate::handles::CaptureHandle;
use crate::types::AudioSourceConfig;

pub use timestamp::monotonic_ms;

/// Running native capture; stopped on [`Drop`] or [`CaptureSession::stop`].
pub trait CaptureSession: Send {
    fn stop(&mut self);
}

/// Starts a single-source capture session that forwards PCM into `handle`.
///
/// [`AudioSourceConfig::Both`] is rejected here — the pipeline opens system +
/// mic sessions separately and runs AEC / mix itself.
///
/// Stubbed capture (`set_capture_stub` / `KOE_STUB_CAPTURE`) never reaches
/// here: [`crate::start_capture`] returns a handle without a native session.
pub fn start_session(handle: Arc<CaptureHandle>) -> Result<Box<dyn CaptureSession>, CaptureError> {
    match handle.source.clone() {
        AudioSourceConfig::Microphone => microphone::start(handle),
        AudioSourceConfig::PidAudio { pid } => process_tap::start(pid, handle),
        AudioSourceConfig::AppAudio { bundle_id } => screen_audio::start(&bundle_id, handle),
        AudioSourceConfig::Both { .. } => Err(CaptureError::Internal {
            msg: "Both must be split by RecordingPipeline into AppAudio + Microphone".to_owned(),
        }),
    }
}
