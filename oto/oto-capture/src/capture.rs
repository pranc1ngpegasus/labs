//! Microphone capture session wrapping [`shiguredo_audio_device::AudioCapture`].

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use shiguredo_audio_device::{AudioCapture, AudioCaptureConfig, AudioFrame, AudioFrameOwned};

use crate::Error;

/// Bounds the requested sample rate (design 02): we always ask the device for
/// 48 kHz and adapt to whatever it actually returns.
const REQUESTED_SAMPLE_RATE: i32 = 48_000;

/// A started capture that forwards frames to a caller-owned bounded channel.
///
/// The capture callback only copies each frame and forwards it over the
/// `SyncSender` (drop-oldest on overflow); all conversion and encoding happens
/// on the pipeline's consumer thread. `dropped` counts frames dropped due to
/// backpressure so the summary can warn about it.
pub struct CaptureSession {
    capture: AudioCapture,
    /// Number of frames dropped because the channel was full.
    dropped: Arc<AtomicUsize>,
}

impl CaptureSession {
    /// Creates and starts a capture session for the given device.
    ///
    /// `device_id` is the `unique_id` of a device from
    /// [`crate::enumerate_input_devices`], or `None` for the default input.
    /// `requested_channels` is the channel count we ask for (1 or 2); the
    /// device may deliver a different number, reported via [`Self::channels`].
    ///
    /// The pipeline owns the bounded channel (design 02): it passes the `sender`
    /// the callback forwards to, and `dropped` to count overflow drops.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the device can't be opened or started.
    pub fn start(
        device_id: Option<String>,
        requested_channels: i32,
        sender: std::sync::mpsc::SyncSender<AudioFrameOwned>,
        dropped: Arc<AtomicUsize>,
    ) -> Result<Self, Error> {
        let callback_dropped = Arc::clone(&dropped);
        let callback = move |frame: AudioFrame<'_>| {
            let frame = frame.to_owned();
            if sender.try_send(frame).is_err() {
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
