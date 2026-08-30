//! System-audio capture — record the output mix (what the system plays).
//!
//! Unlike [`CaptureSession`](crate::CaptureSession), which records a physical
//! input device (a microphone), this captures the audio the system is *playing*
//! (loopback). The source and its implementation differ per platform:
//!
//! - **macOS**: native [ScreenCaptureKit] stream with `capturesAudio` on a
//!   whole-display filter, emitting interleaved Float32 frames. Driver-free,
//!   macOS 13+.
//! - **Linux / Windows**: not yet implemented; [`SystemCaptureSession::start`]
//!   returns [`Error::Unsupported`].
//!
//! The delivered frames are [`AudioFrameOwned`](crate::AudioFrameOwned), so the
//! same consumer pipeline that handles microphone capture also handles system
//! audio unchanged.
//!
//! [ScreenCaptureKit]: https://developer.apple.com/documentation/screencapturekit

#[cfg(target_os = "macos")]
mod macos;

use std::sync::{Arc, atomic::AtomicUsize};

use crate::{AudioFrameOwned, Error};

/// A started capture of the system's output mix.
///
/// Mirrors the [`CaptureSession`](crate::CaptureSession) surface: frames are
/// forwarded to a caller-owned bounded channel with drop-oldest on overflow,
/// and [`Self::stop`] tears down the underlying session.
pub struct SystemCaptureSession {
    #[cfg(target_os = "macos")]
    inner: macos::MacSystemCapture,
}

impl SystemCaptureSession {
    /// Creates and starts a system-audio capture session.
    ///
    /// Frames (interleaved Float32, 48 kHz, stereo) are forwarded to `sender`;
    /// `dropped` counts frames discarded due to backpressure, matching
    /// [`CaptureSession::start`](crate::CaptureSession::start).
    ///
    /// On macOS this must be called on the **main thread** (`ScreenCaptureKit`
    /// requires `AppKit` initialization and a runloop); [`Self::stop`] likewise
    /// completes on the main thread.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] on platforms without a system-audio
    /// backend, or [`Error::Device`] when the session can't be started (e.g.
    /// missing Screen Recording permission, or no display on macOS).
    pub fn start(
        sender: std::sync::mpsc::SyncSender<AudioFrameOwned>,
        dropped: Arc<AtomicUsize>,
    ) -> Result<Self, Error> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                inner: macos::MacSystemCapture::start(sender, dropped)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sender, dropped);
            Err(Error::Unsupported)
        }
    }

    /// Sample rate of the delivered frames.
    #[must_use]
    pub const fn sample_rate(&self) -> i32 {
        #[cfg(target_os = "macos")]
        {
            macos::MacSystemCapture::sample_rate()
        }
        #[cfg(not(target_os = "macos"))]
        {
            #[allow(unreachable_code)]
            0
        }
    }

    /// Channel count of the delivered frames.
    #[must_use]
    pub const fn channels(&self) -> i32 {
        #[cfg(target_os = "macos")]
        {
            macos::MacSystemCapture::channels()
        }
        #[cfg(not(target_os = "macos"))]
        {
            #[allow(unreachable_code)]
            0
        }
    }

    /// Number of frames dropped due to backpressure so far.
    #[must_use]
    // On macOS the body reaches into the session's atomic (not const); on other
    // platforms the cfg-stripped fallback would be const-able, so clippy fires
    // `missing_const_for_fn` only in non-macOS builds.
    #[allow(clippy::missing_const_for_fn)]
    pub fn dropped(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.inner.dropped()
        }
        #[cfg(not(target_os = "macos"))]
        {
            #[allow(unreachable_code)]
            0
        }
    }

    /// Stops the capture, tearing down the underlying system session.
    // See `dropped`: `missing_const_for_fn` fires on non-macOS builds only.
    #[allow(clippy::missing_const_for_fn)]
    pub fn stop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.inner.stop();
        }
    }
}
