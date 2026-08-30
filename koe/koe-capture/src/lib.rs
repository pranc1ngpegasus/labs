//! Koe audio device — capture and playback isolating `shiguredo_audio_device`.
//!
//! Mirrors `oto-capture`'s role: platform-specific backend code and feature
//! selection stay confined to this crate so callers (koe-ffi) work against a
//! small, stable surface. No device enumeration or signal processing lives
//! here — koe always captures the default input, and AGC / Float32 conversion
//! are koe-specific concerns handled by koe-ffi.

#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod playback;

use thiserror::Error;

#[cfg(target_os = "macos")]
pub use capture::CaptureSession;
#[cfg(target_os = "macos")]
pub use playback::PlaybackSession;

/// A single captured audio frame in the backend's raw format.
///
/// On macOS `shiguredo_audio_device` delivers **S16** (signed 16-bit, little
/// endian) interleaved PCM: read it with [`AudioFrameOwned::as_s16`] and
/// divide each sample by `32768.0` to get float. `channels` is the device's
/// *actual* channel count and `sample_rate` the actual rate — both may differ
/// from what you requested, so adapt to them rather than assuming 2ch/48 kHz.
#[cfg(target_os = "macos")]
pub use shiguredo_audio_device::AudioFrameOwned;

/// Sample format identifier carried by [`AudioFrameOwned`].
#[cfg(target_os = "macos")]
pub use shiguredo_audio_device::AudioFormat;

/// Errors from audio device capture and playback.
#[derive(Debug, Error)]
pub enum Error {
    /// Backend open, start, or playback failure.
    #[error("audio device error: {0}")]
    Device(String),
    /// A playback session operation was attempted after the session stopped.
    ///
    /// Returned by [`PlaybackSession::write`] once [`PlaybackSession::stop`]
    /// has been called; never by the capture path.
    #[error("playback session is not running")]
    Stopped,
}
