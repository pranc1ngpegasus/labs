//! Microphone capture session wrapping [`shiguredo_audio_device::AudioCapture`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig, AudioFrame, AudioFrameOwned};

use crate::Error;

/// Bounds the requested sample rate: we always ask the device for 48 kHz and
/// adapt to whatever it actually returns.
const REQUESTED_SAMPLE_RATE: i32 = 48_000;

/// A started capture that forwards frames to a caller-owned bounded channel.
///
/// Frames are delivered in the backend's raw format (`S16` on `CoreAudio`) with
/// the device's actual channel count / sample rate; callers needing the koe
/// canonical format (`f32`, 48 kHz, stereo) convert downstream.
///
/// The channel is bounded (drop-newest: when the consumer falls behind, the
/// most recently arrived frame is discarded so [`CaptureSession::dropped`] can
/// account for backpressure). [`CaptureSession::dropped`] counts frames lost
/// this way.
pub struct CaptureSession {
    capture: AudioCapture,
    /// Number of frames dropped because the channel was full.
    dropped: Arc<AtomicUsize>,
}

impl CaptureSession {
    /// Creates and starts a capture session for the given device.
    ///
    /// `device_id` is a backend device UID, or `None` for the default input.
    /// `requested_channels` is the channel count we ask for (1 or 2); the
    /// device may deliver a different number, reported via [`Self::channels`].
    ///
    /// `dropped` is an atomic counter owned by the caller that is incremented
    /// whenever a frame is discarded because the channel is full.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the device can't be opened or started.
    pub fn start(
        device_id: Option<String>,
        requested_channels: i32,
        sender: SyncSender<AudioFrameOwned>,
        dropped: Arc<AtomicUsize>,
    ) -> Result<Self, Error> {
        let callback_dropped = Arc::clone(&dropped);
        let callback = move |frame: AudioFrame<'_>| {
            // `try_send` keeps the callback non-blocking; a full channel drops
            // the newest arrival, which `callback_dropped` records.
            if sender.try_send(frame.to_owned()).is_err() {
                callback_dropped.fetch_add(1, Ordering::Relaxed);
            }
        };

        let config = AudioCaptureConfig {
            device_id,
            sample_rate: REQUESTED_SAMPLE_RATE,
            channels: requested_channels,
        };
        let mut capture =
            AudioCapture::new(config, callback).map_err(|e| Error::Device(e.to_string()))?;
        capture.start().map_err(|e| Error::Device(e.to_string()))?;

        Ok(Self { capture, dropped })
    }

    /// Actual sample rate reported by the device after start.
    #[must_use]
    pub fn sample_rate(&self) -> i32 {
        self.capture.sample_rate()
    }

    /// Actual channel count reported by the device after start.
    #[must_use]
    pub fn channels(&self) -> i32 {
        self.capture.channels()
    }

    /// Number of frames dropped due to backpressure so far.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops the capture. The channel's senders are dropped by the caller to
    /// close it so the consumer can drain and finalize.
    pub fn stop(&mut self) {
        self.capture.stop();
    }
}
